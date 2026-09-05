// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Attachment, DetachReport, JscBackendFamily, RuntimeError};
use rustjsi_backend::BackendFamily;
use rustjsi_host::{AttachmentId, Host, HostState};
use std::error::Error;
use std::ffi::c_void;
use std::fmt;

/// Foreign-host authority for one legal `JavaScriptCore` entry.
///
/// # Safety
///
/// For every successful call, the implementation must atomically validate that
/// `attachment` is its current `RustJSI` attachment and invoke `operation` exactly
/// once with that attachment's live `JSGlobalContextRef`. It must be on the
/// context's legal thread, hold every VM lock or host synchronization required
/// by `JavaScriptCore`, prevent context destruction until `operation` returns,
/// and restore its entry state if the operation unwinds. It must return an error
/// without invoking `operation` when the identity is stale or any other
/// precondition cannot be established.
pub unsafe trait JscEntrySource {
    /// Failure to establish the foreign host's legal entry.
    type Error: Error;

    /// Runs one operation while the foreign global context is legally entered.
    ///
    /// # Errors
    ///
    /// Returns before invoking `operation` if `attachment` is not current or
    /// host entry cannot be established.
    fn with_global_context<R>(
        &mut self,
        attachment: AttachmentId,
        operation: impl FnOnce(*mut c_void) -> R,
    ) -> Result<R, Self::Error>;
}

/// Error from a source-linked host adapter over a foreign JSC context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JscHostError<E> {
    /// The foreign owner could not establish legal engine entry.
    Entry(E),
    /// The `RustJSI` attachment rejected the context or lifecycle state.
    Runtime(RuntimeError),
}

impl<E: fmt::Display> fmt::Display for JscHostError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entry(error) => write!(formatter, "JSC host entry failed: {error}"),
            Self::Runtime(error) => write!(formatter, "JSC attachment failed: {error}"),
        }
    }
}

impl<E: Error> Error for JscHostError<E> {}

/// Safe source-linked Host view over a foreign-owned JSC attachment.
///
/// The adapter owns neither the attachment nor the JavaScript context. It turns
/// an unsafe, integration-specific [`JscEntrySource`] implementation into the
/// common [`Host`] contract without storing or retaining the raw context.
pub struct JscAttachedHost<'attachment, 'source, S> {
    attachment: &'attachment mut Attachment,
    source: &'source mut S,
}

impl<'attachment, 'source, S> JscAttachedHost<'attachment, 'source, S>
where
    S: JscEntrySource,
{
    /// Borrows attachment state and its foreign host entry source.
    pub fn new(attachment: &'attachment mut Attachment, source: &'source mut S) -> Self {
        Self { attachment, source }
    }

    /// Runs final entry-dependent cleanup without retaining or releasing the
    /// foreign context.
    ///
    /// # Errors
    ///
    /// Returns a host entry failure or the attachment's lifecycle/cleanup error.
    pub fn detach_with_entry(&mut self) -> Result<DetachReport, JscHostError<S::Error>> {
        let attachment = &mut *self.attachment;
        if attachment.state() == HostState::Destroyed {
            return attachment
                .detach_without_context()
                .map_err(JscHostError::Runtime);
        }
        self.source
            .with_global_context(attachment.attachment_id(), |context| {
                // SAFETY: JscEntrySource's unsafe contract establishes every
                // Attachment::detach_with_context precondition for this call.
                unsafe { attachment.detach_with_context(context) }
            })
            .map_err(JscHostError::Entry)?
            .map_err(JscHostError::Runtime)
    }

    /// Detaches when the host cannot provide a final engine entry.
    ///
    /// # Errors
    ///
    /// Returns the attachment's lifecycle failure. A guaranteed-final-entry
    /// attachment rejects this path and remains retryable.
    pub fn detach_without_entry(&mut self) -> Result<DetachReport, JscHostError<S::Error>> {
        self.attachment
            .detach_without_context()
            .map_err(JscHostError::Runtime)
    }
}

impl<S> Host for JscAttachedHost<'_, '_, S>
where
    S: JscEntrySource,
{
    type Family = JscBackendFamily;
    type Error = JscHostError<S::Error>;

    fn attachment_id(&self) -> AttachmentId {
        self.attachment.attachment_id()
    }

    fn state(&self) -> HostState {
        self.attachment.state()
    }

    fn with_backend<R>(
        &mut self,
        operation: impl for<'entry> FnOnce(&mut <Self::Family as BackendFamily>::Backend<'entry>) -> R,
    ) -> Result<R, Self::Error> {
        let attachment = &mut *self.attachment;
        self.source
            .with_global_context(attachment.attachment_id(), |context| {
                // SAFETY: JscEntrySource's unsafe contract establishes every
                // Attachment::with_backend precondition for this call.
                unsafe { attachment.with_backend(context, operation) }
            })
            .map_err(JscHostError::Entry)?
            .map_err(JscHostError::Runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys;
    use rustjsi_backend::{BackendFamily, BackendScope};
    use rustjsi_host::{FinalEntryOutcome, FinalEntryPolicy, RuntimeIdentity};
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr::NonNull;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum EntryDenied {
        Unavailable,
        WrongAttachment,
    }

    impl fmt::Display for EntryDenied {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Unavailable => formatter.write_str("entry unavailable"),
                Self::WrongAttachment => formatter.write_str("wrong attachment"),
            }
        }
    }

    impl Error for EntryDenied {}

    struct ForeignOwner {
        context: NonNull<sys::OpaqueContext>,
        attachment: AttachmentId,
        admit: bool,
        entries: usize,
        active_entries: Cell<usize>,
    }

    impl ForeignOwner {
        fn new(attachment: AttachmentId) -> Self {
            // SAFETY: A null class requests JSC's default global object class.
            let context = unsafe { sys::global_context_create(std::ptr::null_mut()) };
            Self {
                context: NonNull::new(context).expect("JSC test context"),
                attachment,
                admit: true,
                entries: 0,
                active_entries: Cell::new(0),
            }
        }
    }

    struct ActiveEntry<'owner>(&'owner Cell<usize>);

    impl Drop for ActiveEntry<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    // SAFETY: Tests use the owner only on its creating thread. It owns the
    // context through each synchronous operation and restores no external lock.
    unsafe impl JscEntrySource for ForeignOwner {
        type Error = EntryDenied;

        fn with_global_context<R>(
            &mut self,
            attachment: AttachmentId,
            operation: impl FnOnce(*mut c_void) -> R,
        ) -> Result<R, Self::Error> {
            if !self.admit {
                return Err(EntryDenied::Unavailable);
            }
            if attachment != self.attachment {
                return Err(EntryDenied::WrongAttachment);
            }
            self.entries += 1;
            self.active_entries.set(self.active_entries.get() + 1);
            let _active = ActiveEntry(&self.active_entries);
            Ok(operation(self.context.as_ptr().cast()))
        }
    }

    impl Drop for ForeignOwner {
        fn drop(&mut self) {
            // SAFETY: ForeignOwner owns and releases this context exactly once.
            unsafe { sys::global_context_release(self.context.as_ptr()) };
        }
    }

    #[test]
    fn adapter_hides_raw_context_from_generic_host_consumers() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
        let attachment_id = attachment.attachment_id();
        let mut owner = ForeignOwner::new(attachment_id);

        let mut host = JscAttachedHost::new(&mut attachment, &mut owner);
        let number = host
            .with_backend(|backend| {
                JscBackendFamily::try_with_scope(backend, |scope| {
                    let value = scope.evaluate("6 * 7", "attached-host.js")?;
                    scope.as_number(value)
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(number.to_bits(), 42.0_f64.to_bits());
        assert_eq!(host.attachment_id(), attachment_id);
        assert_eq!(
            host.detach_with_entry().unwrap().final_entry(),
            FinalEntryOutcome::Completed
        );
        assert_eq!(host.state(), HostState::Destroyed);
        host.source.admit = false;
        assert_eq!(
            host.detach_with_entry().unwrap().final_entry(),
            FinalEntryOutcome::Completed
        );
        assert_eq!(host.source.entries, 2);
    }

    #[test]
    fn source_rejection_never_runs_the_host_operation() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap();
        let mut owner = ForeignOwner::new(attachment.attachment_id());
        owner.admit = false;
        let mut host = JscAttachedHost::new(&mut attachment, &mut owner);

        assert!(matches!(
            host.with_backend(|_| panic!("denied operation ran")),
            Err(JscHostError::Entry(EntryDenied::Unavailable))
        ));
        assert_eq!(host.state(), HostState::Active);
        assert_eq!(
            host.detach_without_entry().unwrap().final_entry(),
            FinalEntryOutcome::Unavailable
        );
        assert_eq!(host.state(), HostState::Destroyed);
    }

    #[test]
    fn stale_source_cannot_enter_a_replacement_attachment() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut first = Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap();
        let mut owner = ForeignOwner::new(first.attachment_id());

        {
            let mut host = JscAttachedHost::new(&mut first, &mut owner);
            host.with_backend(|_| ()).unwrap();
            let _ = host.detach_without_entry().unwrap();
        }

        let mut replacement = Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap();
        let replacement_id = replacement.attachment_id();
        let mut host = JscAttachedHost::new(&mut replacement, &mut owner);
        assert!(matches!(
            host.with_backend(|_| panic!("stale source entered replacement")),
            Err(JscHostError::Entry(EntryDenied::WrongAttachment))
        ));
        assert_eq!(host.attachment_id(), replacement_id);
        assert_eq!(host.state(), HostState::Active);
        assert_eq!(host.source.entries, 1);
        let _ = host.detach_without_entry().unwrap();
    }

    #[test]
    fn unwind_restores_foreign_entry_before_reentry() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap();
        let mut owner = ForeignOwner::new(attachment.attachment_id());
        let mut host = JscAttachedHost::new(&mut attachment, &mut owner);

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = host.with_backend(|_| panic!("host operation failed"));
        }));
        assert!(panic.is_err());
        assert_eq!(host.source.active_entries.get(), 0);

        host.with_backend(|_| ()).unwrap();
        assert_eq!(host.source.entries, 2);
        assert_eq!(host.source.active_entries.get(), 0);
        let _ = host.detach_without_entry().unwrap();
    }

    #[test]
    fn guaranteed_detach_remains_retryable_after_entry_denial() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
        let mut owner = ForeignOwner::new(attachment.attachment_id());
        owner.admit = false;
        let mut host = JscAttachedHost::new(&mut attachment, &mut owner);

        assert!(matches!(
            host.detach_with_entry(),
            Err(JscHostError::Entry(EntryDenied::Unavailable))
        ));
        assert_eq!(host.state(), HostState::Active);
        assert_eq!(host.source.active_entries.get(), 0);

        host.source.admit = true;
        assert_eq!(
            host.detach_with_entry().unwrap().final_entry(),
            FinalEntryOutcome::Completed
        );
        assert_eq!(host.state(), HostState::Destroyed);
        assert_eq!(host.source.active_entries.get(), 0);
    }
}
