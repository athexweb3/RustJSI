// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{AttachmentId, HostState};
use rustjsi_backend::BackendFamily;
use std::error::Error;

/// Source-linked authority for entering one host-owned engine attachment.
///
/// This is the low-level seam between host lifecycle policy and backend
/// mechanics. An implementation must establish the engine's legal thread,
/// isolate or VM synchronization before calling `operation`. The backend and
/// every value created through it are confined to that call.
///
/// The trait is intentionally generic and is not a stable binary ABI. It adds
/// no required allocation, type erasure, name lookup, or dynamic dispatch to a
/// source-linked entry.
///
/// An entry backend cannot escape:
///
/// ```compile_fail
/// use rustjsi_host::Host;
/// fn escape<H: Host>(host: &mut H) {
///     let _backend = host.with_backend(|backend| backend);
/// }
/// ```
pub trait Host {
    /// Backend entry and scope family lent by this host.
    type Family: BackendFamily;

    /// Entry rejection or host synchronization failure.
    type Error: Error;

    /// Returns the identity of the current engine attachment.
    ///
    /// This snapshot does not establish entry authority. Replacement engines
    /// must receive a new epoch before new work is admitted.
    fn attachment_id(&self) -> AttachmentId;

    /// Returns the current monotonic lifecycle state.
    ///
    /// The value is diagnostic. A caller must still use [`Self::with_backend`]
    /// because the state may change before a separate operation begins.
    fn state(&self) -> HostState;

    /// Lends backend mechanics during one legal host entry.
    ///
    /// Implementations must reject entry unless their attachment is active,
    /// validate the attachment identity, establish all engine synchronization,
    /// and restore entry state if `operation` unwinds. They must not extend the
    /// engine lifetime or synthesize a second runtime entry.
    ///
    /// # Errors
    ///
    /// Returns before running `operation` when legal engine entry cannot be
    /// established.
    fn with_backend<R>(
        &mut self,
        operation: impl for<'entry> FnOnce(&mut <Self::Family as BackendFamily>::Backend<'entry>) -> R,
    ) -> Result<R, Self::Error>;
}

impl<H> Host for &mut H
where
    H: Host + ?Sized,
{
    type Family = H::Family;
    type Error = H::Error;

    fn attachment_id(&self) -> AttachmentId {
        H::attachment_id(self)
    }

    fn state(&self) -> HostState {
        H::state(self)
    }

    fn with_backend<R>(
        &mut self,
        operation: impl for<'entry> FnOnce(&mut <Self::Family as BackendFamily>::Backend<'entry>) -> R,
    ) -> Result<R, Self::Error> {
        H::with_backend(self, operation)
    }
}
