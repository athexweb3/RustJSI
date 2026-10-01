// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;
use std::fmt;

use rustjsi_host::{AttachmentId, RuntimeId};

/// Dispatchability state for one host-owned attachment registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainRegistrationState {
    /// The registered attachment may resolve matching drain tasks.
    Active,
    /// The registration remains identifiable but rejects matching drain tasks.
    Closing,
}

/// Resolution of an attachment-only scheduler task by its host owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainTaskResolution {
    /// The task names the current active attachment.
    Current,
    /// The current attachment is closing and cannot dispatch new work.
    Closing,
    /// The task names an earlier attachment for this logical runtime.
    Retired,
    /// The task names a later or otherwise unregistered attachment epoch.
    UnregisteredAttachment,
    /// The task names another logical runtime.
    ForeignRuntime,
}

/// Failure to replace a host-owned attachment registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainRegistrationReplaceError {
    /// Replacement requires the old registration to begin close first.
    NotClosing(DrainRegistrationState),
    /// A replacement must remain in the same logical runtime.
    ForeignRuntime,
    /// A replacement must advance the attachment epoch monotonically.
    NotNewer,
}

/// Deterministic owner model for one logical runtime's drain registration.
///
/// It owns identity resolution only. It contains no scheduler queue, host entry,
/// backend, mailbox payload, platform wake-up, or terminal cleanup policy.
#[derive(Debug)]
pub struct DrainRegistrationModel {
    current: AttachmentId,
    state: DrainRegistrationState,
}

impl DrainRegistrationModel {
    /// Creates an active registration for an owner-issued attachment.
    #[must_use]
    pub const fn new(attachment: AttachmentId) -> Self {
        Self {
            current: attachment,
            state: DrainRegistrationState::Active,
        }
    }

    /// Returns the logical runtime this owner models.
    #[must_use]
    pub const fn runtime_id(&self) -> RuntimeId {
        self.current.runtime_id()
    }

    /// Returns the attachment currently registered by this owner.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.current
    }

    /// Returns the current registration state.
    #[must_use]
    pub const fn state(&self) -> DrainRegistrationState {
        self.state
    }

    /// Stops dispatching new drain tasks for the current attachment.
    pub fn begin_close(&mut self) {
        self.state = DrainRegistrationState::Closing;
    }

    /// Replaces a closing attachment with a strictly newer epoch.
    ///
    /// # Errors
    ///
    /// Rejects foreign, non-monotonic, or still-active replacements.
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

    /// Resolves one attachment-only task without entering a host or backend.
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
    use rustjsi_host::RuntimeIdentity;

    #[test]
    fn close_and_replacement_keep_old_tasks_out_of_the_new_attachment() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let first = identity.next_attachment().unwrap();
        let replacement = identity.next_attachment().unwrap();
        let mut model = DrainRegistrationModel::new(first);

        assert_eq!(model.resolve(first), DrainTaskResolution::Current);
        model.begin_close();
        assert_eq!(model.resolve(first), DrainTaskResolution::Closing);
        model.replace(replacement).unwrap();

        assert_eq!(model.resolve(first), DrainTaskResolution::Retired);
        assert_eq!(model.resolve(replacement), DrainTaskResolution::Current);
    }

    #[test]
    fn replacement_rejects_active_foreign_and_non_monotonic_attachments() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let first = identity.next_attachment().unwrap();
        let second = identity.next_attachment().unwrap();
        let foreign = RuntimeIdentity::allocate()
            .unwrap()
            .next_attachment()
            .unwrap();
        let mut model = DrainRegistrationModel::new(first);

        assert_eq!(
            model.replace(second),
            Err(DrainRegistrationReplaceError::NotClosing(
                DrainRegistrationState::Active
            ))
        );
        model.begin_close();
        assert_eq!(
            model.replace(foreign),
            Err(DrainRegistrationReplaceError::ForeignRuntime)
        );
        assert_eq!(
            model.replace(first),
            Err(DrainRegistrationReplaceError::NotNewer)
        );
    }

    #[test]
    fn foreign_and_future_tasks_never_resolve_as_current() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let current = identity.next_attachment().unwrap();
        let future = identity.next_attachment().unwrap();
        let foreign = RuntimeIdentity::allocate()
            .unwrap()
            .next_attachment()
            .unwrap();
        let model = DrainRegistrationModel::new(current);

        assert_eq!(
            model.resolve(future),
            DrainTaskResolution::UnregisteredAttachment
        );
        assert_eq!(model.resolve(foreign), DrainTaskResolution::ForeignRuntime);
    }
}
