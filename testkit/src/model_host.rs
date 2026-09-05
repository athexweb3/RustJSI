// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{ModelBackend, ModelBackendFamily};
use rustjsi_backend::BackendFamily;
use rustjsi_host::{
    AttachmentId, EntryGate, FinalEntryOutcome, FinalEntryPolicy, GateError, Host, HostState,
    IdentityError, RuntimeIdentity,
};
use std::num::NonZeroU32;

const MODEL_ENTRY_LIMIT: NonZeroU32 = NonZeroU32::new(64).unwrap();

/// Deterministic owner that separates host entry from model backend mechanics.
///
/// This fixture has no engine or VM lock. It applies the same attachment
/// identity, entry confinement, and monotonic admission rules expected from a
/// real source-linked host.
#[derive(Debug)]
pub struct ModelHost {
    attachment: AttachmentId,
    gate: EntryGate,
    backend: ModelBackend,
}

impl ModelHost {
    /// Creates an active model host with a unique logical runtime and attachment.
    ///
    /// # Errors
    ///
    /// Returns an identity error if the process-wide host identity domain is
    /// exhausted.
    pub fn new() -> Result<Self, IdentityError> {
        let mut identity = RuntimeIdentity::allocate()?;
        Ok(Self::for_attachment(identity.next_attachment()?))
    }

    /// Creates an active model host for an owner-issued attachment identity.
    #[must_use]
    pub fn for_attachment(attachment: AttachmentId) -> Self {
        Self {
            attachment,
            gate: EntryGate::new(MODEL_ENTRY_LIMIT, FinalEntryPolicy::BestEffort),
            backend: ModelBackend::new(),
        }
    }

    /// Stops admitting model entries.
    pub fn request_drain(&self) {
        self.gate.request_drain();
    }

    /// Completes a drain with no engine-dependent cleanup.
    ///
    /// # Errors
    ///
    /// Returns the gate's lifecycle failure if entries remain or the transition
    /// is not legal.
    pub fn finish_drain(&self) -> Result<FinalEntryOutcome, GateError> {
        self.gate.finish_drain()
    }

    /// Marks the model attachment destroyed after invalidation.
    ///
    /// # Errors
    ///
    /// Returns the gate's lifecycle failure if draining has not completed.
    pub fn mark_destroyed(&self) -> Result<(), GateError> {
        self.gate.mark_destroyed()
    }
}

impl Host for ModelHost {
    type Family = ModelBackendFamily;
    type Error = GateError;

    fn attachment_id(&self) -> AttachmentId {
        self.attachment
    }

    fn state(&self) -> HostState {
        self.gate.state()
    }

    fn with_backend<R>(
        &mut self,
        operation: impl for<'entry> FnOnce(&mut <Self::Family as BackendFamily>::Backend<'entry>) -> R,
    ) -> Result<R, Self::Error> {
        let _entry = self.gate.try_enter()?;
        Ok(self.backend.with_entry(operation))
    }
}
