// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::{AttachmentId, RuntimeId};

/// Dispatchability state of one host-owned attachment registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainRegistrationState {
    /// The registered attachment may accept matching drain tasks.
    Active,
    /// The registered attachment remains identifiable but accepts no new task dispatch.
    Closing,
}

/// Host-owner resolution of an attachment-only drain task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainTaskResolution {
    /// The task targets the active registered attachment.
    Current,
    /// The task targets the current attachment while it is closing.
    Closing,
    /// The task targets an earlier attachment of this logical runtime.
    Retired,
    /// The task targets a later or otherwise unregistered attachment epoch.
    UnregisteredAttachment,
    /// The task targets another logical runtime.
    ForeignRuntime,
}

/// Failure to replace a host-owned drain registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainRegistrationReplaceError {
    /// Replacement requires the old registration to begin closing first.
    NotClosing(DrainRegistrationState),
    /// A replacement must remain in the same logical runtime.
    ForeignRuntime,
    /// A replacement must advance the attachment epoch monotonically.
    NotNewer,
}

/// Identity-only host registration for future attachment drain tasks.
///
/// A host owner resolves platform-delivered task identities through this type
/// before choosing a mailbox or attempting host entry. It owns neither a
/// scheduler nor a work queue. In particular, beginning close here does not
/// close producer ingress or settle retained payloads; an owning mailbox keeps
/// those responsibilities. The registration is thread-affine: producers post
/// copyable [`AttachmentId`] values, while the host-owner thread resolves them.
///
/// ```compile_fail
/// use rustjsi_host::{DrainRegistration, RuntimeIdentity};
///
/// fn requires_send<T: Send>(_: T) {}
///
/// let mut identity = RuntimeIdentity::allocate().unwrap();
/// let attachment = identity.next_attachment().unwrap();
/// requires_send(DrainRegistration::new(attachment));
/// ```
#[derive(Debug)]
pub struct DrainRegistration {
    current: AttachmentId,
    state: DrainRegistrationState,
    _affine: PhantomData<Rc<()>>,
}

impl DrainRegistration {
    /// Creates an active registration for an owner-issued attachment.
    #[must_use]
    pub const fn new(attachment: AttachmentId) -> Self {
        Self {
            current: attachment,
            state: DrainRegistrationState::Active,
            _affine: PhantomData,
        }
    }

    /// Returns the logical runtime this registration represents.
    #[must_use]
    pub const fn runtime_id(&self) -> RuntimeId {
        self.current.runtime_id()
    }

    /// Returns the attachment currently registered by this host owner.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.current
    }

    /// Returns the current dispatchability state.
    #[must_use]
    pub const fn state(&self) -> DrainRegistrationState {
        self.state
    }

    /// Stops accepting new task dispatch for the current attachment.
    pub fn begin_close(&mut self) {
        self.state = DrainRegistrationState::Closing;
    }

    /// Replaces a closing attachment with a strictly newer epoch.
    ///
    /// # Errors
    ///
    /// Rejects replacement while active, from another runtime, or without a
    /// monotonic attachment epoch advance. It does not close or drain an
    /// associated work mailbox.
    pub fn replace(
        &mut self,
        replacement: AttachmentId,
    ) -> Result<(), DrainRegistrationReplaceError> {
        if self.state != DrainRegistrationState::Closing {
            return Err(DrainRegistrationReplaceError::NotClosing(self.state));
        }
        if replacement.runtime_id() != self.current.runtime_id() {
            return Err(DrainRegistrationReplaceError::ForeignRuntime);
        }
        if replacement.epoch() <= self.current.epoch() {
            return Err(DrainRegistrationReplaceError::NotNewer);
        }
        self.current = replacement;
        self.state = DrainRegistrationState::Active;
        Ok(())
    }

    /// Resolves one attachment-only task before mailbox selection or host entry.
    #[must_use]
    pub fn resolve(&self, attachment: AttachmentId) -> DrainTaskResolution {
        if attachment.runtime_id() != self.current.runtime_id() {
            return DrainTaskResolution::ForeignRuntime;
        }
        if attachment.epoch() < self.current.epoch() {
            return DrainTaskResolution::Retired;
        }
        if attachment != self.current {
            return DrainTaskResolution::UnregisteredAttachment;
        }
        match self.state {
            DrainRegistrationState::Active => DrainTaskResolution::Current,
            DrainRegistrationState::Closing => DrainTaskResolution::Closing,
        }
    }
}

impl fmt::Display for DrainRegistrationReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotClosing(state) => write!(formatter, "cannot replace {state:?} registration"),
            Self::ForeignRuntime => formatter.write_str("replacement belongs to another runtime"),
            Self::NotNewer => formatter.write_str("replacement attachment epoch is not newer"),
        }
    }
}

impl Error for DrainRegistrationReplaceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeIdentity;

    #[test]
    fn replacement_retires_old_tasks_without_targeting_the_new_attachment() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let first = identity.next_attachment().unwrap();
        let replacement = identity.next_attachment().unwrap();
        let mut registration = DrainRegistration::new(first);

        assert_eq!(registration.resolve(first), DrainTaskResolution::Current);
        registration.begin_close();
        assert_eq!(registration.resolve(first), DrainTaskResolution::Closing);
        registration.replace(replacement).unwrap();

        assert_eq!(registration.resolve(first), DrainTaskResolution::Retired);
        assert_eq!(
            registration.resolve(replacement),
            DrainTaskResolution::Current
        );
    }

    #[test]
    fn replacement_requires_closing_and_monotonic_same_runtime_identity() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let first = identity.next_attachment().unwrap();
        let second = identity.next_attachment().unwrap();
        let foreign = RuntimeIdentity::allocate()
            .unwrap()
            .next_attachment()
            .unwrap();
        let mut registration = DrainRegistration::new(first);

        assert_eq!(
            registration.replace(second),
            Err(DrainRegistrationReplaceError::NotClosing(
                DrainRegistrationState::Active
            ))
        );
        registration.begin_close();
        assert_eq!(
            registration.replace(foreign),
            Err(DrainRegistrationReplaceError::ForeignRuntime)
        );
        assert_eq!(
            registration.replace(first),
            Err(DrainRegistrationReplaceError::NotNewer)
        );
    }

    #[test]
    fn foreign_and_future_tasks_are_never_current() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let current = identity.next_attachment().unwrap();
        let future = identity.next_attachment().unwrap();
        let foreign = RuntimeIdentity::allocate()
            .unwrap()
            .next_attachment()
            .unwrap();
        let registration = DrainRegistration::new(current);

        assert_eq!(
            registration.resolve(future),
            DrainTaskResolution::UnregisteredAttachment
        );
        assert_eq!(
            registration.resolve(foreign),
            DrainTaskResolution::ForeignRuntime
        );
    }
}
