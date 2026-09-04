// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// Identity of one logical host runtime within a linked host allocator domain.
///
/// Replacing an engine attached to the same logical runtime preserves this ID
/// and receives a new [`AttachmentEpoch`]. IDs are never recycled.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeId(NonZeroU64);

impl RuntimeId {
    /// Returns the nonzero integer identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic generation of an engine attachment to one logical runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttachmentEpoch(NonZeroU64);

impl AttachmentEpoch {
    /// Returns the nonzero generation number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Stable identity of exactly one engine attachment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttachmentId {
    runtime_id: RuntimeId,
    epoch: AttachmentEpoch,
}

impl AttachmentId {
    /// Returns the logical runtime identity.
    #[must_use]
    pub const fn runtime_id(self) -> RuntimeId {
        self.runtime_id
    }

    /// Returns this attachment's generation.
    #[must_use]
    pub const fn epoch(self) -> AttachmentEpoch {
        self.epoch
    }
}

/// Owner-held source of attachment identities for one logical runtime.
///
/// A host keeps this value across engine replacement and requests the next
/// attachment ID whenever it installs a new engine. The source is deliberately
/// neither `Clone` nor constructible from caller-selected integers, preventing
/// safe code from creating duplicate identities.
///
/// ```compile_fail
/// use rustjsi_host::RuntimeIdentity;
/// let identity = RuntimeIdentity::allocate().unwrap();
/// let duplicate = identity.clone();
/// drop(duplicate);
/// ```
#[derive(Debug)]
pub struct RuntimeIdentity {
    runtime_id: RuntimeId,
    next_epoch: Option<NonZeroU64>,
}

impl RuntimeIdentity {
    /// Allocates a new logical runtime identity in this linked host domain.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::RuntimeIdExhausted`] after the allocator's ID
    /// space is exhausted. Exhausted IDs are never recycled.
    pub fn allocate() -> Result<Self, IdentityError> {
        let raw = NEXT_RUNTIME_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| IdentityError::RuntimeIdExhausted)?;
        let runtime_id =
            RuntimeId(NonZeroU64::new(raw).expect("runtime identity allocator starts at one"));
        Ok(Self {
            runtime_id,
            next_epoch: NonZeroU64::new(1),
        })
    }

    /// Returns the stable logical runtime identity.
    #[must_use]
    pub const fn runtime_id(&self) -> RuntimeId {
        self.runtime_id
    }

    /// Issues the next attachment generation without recycling older epochs.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::AttachmentEpochExhausted`] once every nonzero
    /// epoch has been issued for this logical runtime.
    pub fn next_attachment(&mut self) -> Result<AttachmentId, IdentityError> {
        let epoch = self
            .next_epoch
            .ok_or(IdentityError::AttachmentEpochExhausted(self.runtime_id))?;
        self.next_epoch = epoch.get().checked_add(1).and_then(NonZeroU64::new);
        Ok(AttachmentId {
            runtime_id: self.runtime_id,
            epoch: AttachmentEpoch(epoch),
        })
    }
}

/// Failure to issue a unique runtime or attachment identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The linked host allocator's logical runtime identity space is exhausted.
    RuntimeIdExhausted,
    /// The attachment generation space for a logical runtime is exhausted.
    AttachmentEpochExhausted(RuntimeId),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeIdExhausted => formatter.write_str("runtime identity space exhausted"),
            Self::AttachmentEpochExhausted(runtime_id) => write!(
                formatter,
                "attachment epoch space exhausted for runtime {}",
                runtime_id.get()
            ),
        }
    }
}

impl Error for IdentityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_runtime_ids_are_unique() {
        let first = RuntimeIdentity::allocate().unwrap();
        let second = RuntimeIdentity::allocate().unwrap();
        assert_ne!(first.runtime_id(), second.runtime_id());
    }

    #[test]
    fn replacement_preserves_runtime_and_advances_epoch() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let first = identity.next_attachment().unwrap();
        let second = identity.next_attachment().unwrap();

        assert_eq!(first.runtime_id(), second.runtime_id());
        assert_eq!(first.epoch().get(), 1);
        assert_eq!(second.epoch().get(), 2);
        assert_ne!(first, second);
    }

    #[test]
    fn epoch_exhaustion_does_not_recycle_an_attachment() {
        let runtime_id = RuntimeId(NonZeroU64::new(7).unwrap());
        let mut identity = RuntimeIdentity {
            runtime_id,
            next_epoch: NonZeroU64::new(u64::MAX),
        };

        let last = identity.next_attachment().unwrap();
        assert_eq!(last.epoch().get(), u64::MAX);
        assert_eq!(
            identity.next_attachment(),
            Err(IdentityError::AttachmentEpochExhausted(runtime_id))
        );
    }
}
