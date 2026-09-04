// SPDX-License-Identifier: MIT OR Apache-2.0

pub use rustjsi_host::{AttachmentEpoch as Epoch, AttachmentId, RuntimeId};
use std::error::Error;
use std::fmt;

/// Monotonic state of a modeled host-owned runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    /// New legal entries and work are accepted.
    Active,
    /// New work is rejected while existing entries drain.
    Draining,
    /// Engine entry is no longer legal.
    Invalid,
    /// Backend state has been detached and destroyed.
    Destroyed,
}

/// One active entry token in the deterministic lifecycle model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    attachment: AttachmentId,
    sequence: u64,
}

impl Entry {
    /// Returns the runtime ID captured at entry.
    #[must_use]
    pub const fn runtime_id(self) -> RuntimeId {
        self.attachment.runtime_id()
    }

    /// Returns the epoch captured at entry.
    #[must_use]
    pub const fn epoch(self) -> Epoch {
        self.attachment.epoch()
    }

    /// Returns the complete attachment identity captured at entry.
    #[must_use]
    pub const fn attachment_id(self) -> AttachmentId {
        self.attachment
    }
}

/// A transition emitted by [`LifecycleModel`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
    /// A legal runtime entry began.
    Entered(Entry),
    /// An active entry ended.
    Exited(Entry),
    /// Invalidation changed the runtime to draining.
    DrainRequested,
    /// The last active entry left a draining runtime.
    DrainReady,
    /// The host completed its authorized drain and invalidated engine entry.
    Invalidated,
    /// Backend state was detached and destroyed.
    Destroyed,
}

/// A deterministic lifecycle operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    /// The runtime is not active enough for a new entry.
    NotActive(RuntimeState),
    /// An entry belongs to a different runtime or attachment epoch.
    WrongRuntime,
    /// An entry token is stale or has already exited.
    StaleEntry,
    /// No further unique entry sequence can be issued safely.
    EntrySpaceExhausted,
    /// The drain still has active entries.
    EntriesRemain(u32),
    /// The requested transition is illegal from the current state.
    InvalidTransition {
        /// State at the time of the request.
        state: RuntimeState,
        /// Requested operation.
        operation: &'static str,
    },
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotActive(state) => write!(formatter, "runtime is not active: {state:?}"),
            Self::WrongRuntime => formatter.write_str("entry belongs to another runtime epoch"),
            Self::StaleEntry => formatter.write_str("entry is stale"),
            Self::EntrySpaceExhausted => formatter.write_str("entry sequence space exhausted"),
            Self::EntriesRemain(count) => write!(formatter, "{count} active entries remain"),
            Self::InvalidTransition { state, operation } => {
                write!(formatter, "cannot {operation} from {state:?}")
            }
        }
    }
}

impl Error for LifecycleError {}

/// Pure deterministic model of the host-owned runtime lifecycle.
///
/// It contains no engine handles and grants no thread-entry authority. Tests use
/// it to exhaust transition ordering before a production host implementation is
/// introduced.
#[derive(Debug)]
pub struct LifecycleModel {
    attachment: AttachmentId,
    state: RuntimeState,
    next_entry: u64,
    live_entries: Vec<u64>,
    events: Vec<LifecycleEvent>,
}

impl LifecycleModel {
    /// Creates an active attachment.
    #[must_use]
    pub fn new(attachment: AttachmentId) -> Self {
        Self {
            attachment,
            state: RuntimeState::Active,
            next_entry: 1,
            live_entries: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Returns the attachment identity modeled by this lifecycle instance.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.attachment
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> RuntimeState {
        self.state
    }

    /// Returns the number of entries not yet exited.
    #[must_use]
    pub fn active_entries(&self) -> u32 {
        u32::try_from(self.live_entries.len()).unwrap_or(u32::MAX)
    }

    /// Returns the complete transition trace.
    #[must_use]
    pub fn events(&self) -> &[LifecycleEvent] {
        &self.events
    }

    /// Begins a legal host entry.
    ///
    /// # Errors
    ///
    /// Rejects entry after draining has begun.
    pub fn enter(&mut self) -> Result<Entry, LifecycleError> {
        if self.state != RuntimeState::Active {
            return Err(LifecycleError::NotActive(self.state));
        }
        let next_entry = self
            .next_entry
            .checked_add(1)
            .ok_or(LifecycleError::EntrySpaceExhausted)?;
        let entry = Entry {
            attachment: self.attachment,
            sequence: self.next_entry,
        };
        self.next_entry = next_entry;
        self.live_entries.push(entry.sequence);
        self.events.push(LifecycleEvent::Entered(entry));
        Ok(entry)
    }

    /// Ends a matching active entry.
    ///
    /// # Errors
    ///
    /// Rejects foreign, stale, or duplicate entry tokens.
    pub fn exit(&mut self, entry: Entry) -> Result<(), LifecycleError> {
        if entry.attachment != self.attachment {
            return Err(LifecycleError::WrongRuntime);
        }
        let Some(index) = self
            .live_entries
            .iter()
            .position(|sequence| *sequence == entry.sequence)
        else {
            return Err(LifecycleError::StaleEntry);
        };
        self.live_entries.swap_remove(index);
        self.events.push(LifecycleEvent::Exited(entry));
        if self.state == RuntimeState::Draining && self.live_entries.is_empty() {
            self.events.push(LifecycleEvent::DrainReady);
        }
        Ok(())
    }

    /// Requests monotonic, idempotent invalidation.
    pub fn request_invalidate(&mut self) {
        if self.state == RuntimeState::Active {
            self.state = RuntimeState::Draining;
            self.events.push(LifecycleEvent::DrainRequested);
            if self.live_entries.is_empty() {
                self.events.push(LifecycleEvent::DrainReady);
            }
        }
    }

    /// Completes the host-authorized drain.
    ///
    /// # Errors
    ///
    /// Requires draining state with no active entries.
    pub fn finish_drain(&mut self) -> Result<(), LifecycleError> {
        match self.state {
            RuntimeState::Invalid => return Ok(()),
            RuntimeState::Draining => {}
            state => {
                return Err(LifecycleError::InvalidTransition {
                    state,
                    operation: "finish drain",
                });
            }
        }
        if !self.live_entries.is_empty() {
            return Err(LifecycleError::EntriesRemain(self.active_entries()));
        }
        self.state = RuntimeState::Invalid;
        self.events.push(LifecycleEvent::Invalidated);
        Ok(())
    }

    /// Marks backend detach/destruction complete.
    ///
    /// # Errors
    ///
    /// Requires an invalid runtime. Repeated destruction is accepted.
    pub fn destroy(&mut self) -> Result<(), LifecycleError> {
        match self.state {
            RuntimeState::Destroyed => Ok(()),
            RuntimeState::Invalid => {
                self.state = RuntimeState::Destroyed;
                self.events.push(LifecycleEvent::Destroyed);
                Ok(())
            }
            state => Err(LifecycleError::InvalidTransition {
                state,
                operation: "destroy",
            }),
        }
    }

    /// Verifies that work targets this active attachment.
    ///
    /// # Errors
    ///
    /// Rejects stale identity/epoch and all work once draining starts.
    pub fn validate_work(&self, attachment: AttachmentId) -> Result<(), LifecycleError> {
        if attachment != self.attachment {
            return Err(LifecycleError::WrongRuntime);
        }
        if self.state == RuntimeState::Active {
            Ok(())
        } else {
            Err(LifecycleError::NotActive(self.state))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustjsi_host::RuntimeIdentity;

    fn new_attachment() -> AttachmentId {
        RuntimeIdentity::allocate()
            .unwrap()
            .next_attachment()
            .unwrap()
    }

    #[test]
    fn invalidation_waits_for_outermost_entry() {
        let mut model = LifecycleModel::new(new_attachment());
        let outer = model.enter().unwrap();
        let inner = model.enter().unwrap();

        model.request_invalidate();
        assert_eq!(model.state(), RuntimeState::Draining);
        assert!(matches!(model.enter(), Err(LifecycleError::NotActive(_))));
        assert_eq!(model.finish_drain(), Err(LifecycleError::EntriesRemain(2)));

        model.exit(inner).unwrap();
        assert_eq!(model.finish_drain(), Err(LifecycleError::EntriesRemain(1)));
        model.exit(outer).unwrap();
        model.finish_drain().unwrap();
        model.destroy().unwrap();
        assert_eq!(model.state(), RuntimeState::Destroyed);
    }

    #[test]
    fn stale_epoch_work_is_rejected() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let stale = identity.next_attachment().unwrap();
        let current = identity.next_attachment().unwrap();
        let model = LifecycleModel::new(current);
        assert_eq!(model.attachment_id(), current);
        assert_eq!(
            model.validate_work(stale),
            Err(LifecycleError::WrongRuntime)
        );

        let foreign = new_attachment();
        assert_eq!(
            model.validate_work(foreign),
            Err(LifecycleError::WrongRuntime)
        );
    }

    #[test]
    fn one_thousand_lifecycle_cycles_are_finite_and_idempotent() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        for _ in 0..1_000 {
            let mut model = LifecycleModel::new(identity.next_attachment().unwrap());
            let entry = model.enter().unwrap();
            model.request_invalidate();
            model.request_invalidate();
            model.exit(entry).unwrap();
            model.finish_drain().unwrap();
            model.finish_drain().unwrap();
            model.destroy().unwrap();
            model.destroy().unwrap();
            assert_eq!(model.state(), RuntimeState::Destroyed);
            assert_eq!(model.active_entries(), 0);
        }
    }

    #[test]
    fn entry_sequence_exhaustion_fails_closed() {
        let mut model = LifecycleModel::new(new_attachment());
        model.next_entry = u64::MAX;
        assert_eq!(model.enter(), Err(LifecycleError::EntrySpaceExhausted));
        assert_eq!(model.active_entries(), 0);
    }
}
