// SPDX-License-Identifier: MIT OR Apache-2.0

use super::local_roots::LocalRoots;
use super::{ActiveRuntimeGuard, Context, RootLimits, RuntimeError, Shared, sys};
use rustjsi_host::{AttachmentId, FinalEntryOutcome, FinalEntryPolicy, HostState, RuntimeIdentity};
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

/// `RustJSI` state attached to a `JavaScriptCore` context owned by another host.
///
/// An attachment never retains, releases, or stores the host's global context.
/// The host must lend the live context for every entry and for final cleanup.
///
/// ```compile_fail
/// use rustjsi_backend_jsc::Attachment;
/// fn require_send<T: Send>() {}
/// require_send::<Attachment>();
/// ```
///
/// ```compile_fail
/// use rustjsi_backend_jsc::Attachment;
/// use rustjsi_host::{FinalEntryPolicy, RuntimeIdentity};
/// let mut identity = RuntimeIdentity::allocate().unwrap();
/// let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap();
/// let local = unsafe {
///     attachment.with_context(std::ptr::null_mut(), |cx| cx.eval("({})", "escape.js").unwrap())
/// }.unwrap();
/// drop(local);
/// ```
pub struct Attachment {
    pub(super) shared: Rc<Shared>,
}

/// Resource accounting returned when a foreign JSC attachment is detached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub struct DetachReport {
    final_entry: FinalEntryOutcome,
    released_persistent_roots: usize,
    unresolved_persistent_roots: usize,
    released_host_functions: usize,
    unresolved_host_functions: usize,
    retired_native_states: usize,
    remaining_external_allocations: usize,
    remaining_external_bytes: usize,
    callback_drop_panics: usize,
    native_state_drop_panics: usize,
}

impl Attachment {
    /// Creates attachment state without creating or retaining a JSC context.
    ///
    /// The identity issuer must belong to the host's logical runtime. Replacing
    /// an attachment should reuse the issuer so the epoch advances.
    ///
    /// # Errors
    ///
    /// Returns an identity error if the issuer cannot allocate another epoch.
    pub fn new(
        identity: &mut RuntimeIdentity,
        final_entry_policy: FinalEntryPolicy,
    ) -> Result<Self, RuntimeError> {
        Self::new_with_root_limits(identity, final_entry_policy, RootLimits::default())
    }

    /// Creates attachment state with independent persistent and local root budgets.
    ///
    /// # Errors
    ///
    /// Returns an identity error if the issuer cannot allocate another epoch.
    pub fn new_with_root_limits(
        identity: &mut RuntimeIdentity,
        final_entry_policy: FinalEntryPolicy,
        limits: RootLimits,
    ) -> Result<Self, RuntimeError> {
        let id = identity
            .next_attachment()
            .map_err(|_| RuntimeError::IdentityExhausted)?;
        Ok(Self {
            shared: Shared::new(id, final_entry_policy, limits),
        })
    }

    /// Enters the host-owned context for the duration of `operation`.
    ///
    /// The returned `Context` and all scoped values are unable to escape.
    ///
    /// # Safety
    ///
    /// `context` must be this attachment's live `JSGlobalContextRef`, not an
    /// arbitrary `JSContextRef`. The caller must be on the context's legal thread
    /// and hold every VM lock or host synchronization required by
    /// `JavaScriptCore`. The host must prevent context destruction for the full
    /// call and must never use this attachment with a replacement or different
    /// context.
    ///
    /// # Errors
    ///
    /// Returns an affinity, lifecycle, admission, or null-context error.
    pub unsafe fn with_context<R>(
        &mut self,
        context: *mut c_void,
        operation: impl for<'cx> FnOnce(&mut Context<'cx>) -> R,
    ) -> Result<R, RuntimeError> {
        self.shared.ensure_active()?;
        let raw = borrowed_global_context(context)?;
        let _entry = self.shared.gate.try_enter().map_err(RuntimeError::Host)?;
        let active = ActiveRuntimeGuard::enter(Rc::as_ptr(&self.shared), raw);
        self.shared.drain_native_finalizers();
        self.shared.drain_root_releases(raw);
        let result = {
            let mut scoped = Context {
                shared: &self.shared,
                raw,
                local_roots: std::cell::RefCell::new(LocalRoots::new()),
                scope_depth: 0,
                _affine: PhantomData,
            };
            operation(&mut scoped)
        };
        self.shared.drain_native_finalizers();
        self.shared.drain_root_releases(raw);
        drop(active);
        Ok(result)
    }

    /// Stops admitting work and releases engine roots during a final host entry.
    ///
    /// This does not retain or release the JSC context. The host remains its owner
    /// and may continue using it after this method returns.
    ///
    /// # Safety
    ///
    /// `context` has the same requirements as [`Self::with_context`]. It must name
    /// the same global context used for every earlier entry. The host must keep
    /// engine entry legal until all cleanup in this call has completed.
    ///
    /// # Errors
    ///
    /// Returns an affinity, lifecycle, final-entry-policy, or null-context error.
    pub unsafe fn detach_with_context(
        &mut self,
        context: *mut c_void,
    ) -> Result<DetachReport, RuntimeError> {
        self.shared.ensure_thread()?;
        if self.shared.gate.state() == HostState::Destroyed {
            return Ok(self.empty_report());
        }

        self.shared.gate.request_drain();
        let cleanup = self
            .shared
            .gate
            .try_begin_cleanup()
            .map_err(RuntimeError::Host)?;
        let raw = borrowed_global_context(context)?;
        let roots = self.shared.roots.borrow_mut().drain();
        let functions = std::mem::take(&mut *self.shared.host_functions.borrow_mut());
        let released_persistent_roots = roots.len();
        let released_host_functions = functions.len();

        for value in roots
            .into_iter()
            .chain(functions.values().map(|entry| entry.function))
        {
            // SAFETY: The host guarantees this is the attachment's live context.
            // Every registry entry owns exactly one protection in that context.
            unsafe { sys::value_unprotect(raw.as_ptr(), value.as_ptr()) };
        }
        for entry in functions.into_values() {
            self.shared.drop_callback(entry);
        }
        self.shared.close_native_finalizers();
        cleanup.complete();
        let final_entry = self
            .shared
            .gate
            .finish_drain()
            .map_err(RuntimeError::Host)?;
        let retired_native_states = self.retire_native_states();
        self.shared
            .gate
            .mark_destroyed()
            .map_err(RuntimeError::Host)?;

        Ok(self.report(
            final_entry,
            released_persistent_roots,
            0,
            released_host_functions,
            0,
            retired_native_states,
        ))
    }

    /// Detaches when the host cannot provide a final JSC entry.
    ///
    /// Protected values cannot be unprotected without legal engine access. Their
    /// counts are returned as unresolved; the host must destroy the JSC context
    /// before interpreting context destruction as complete cleanup.
    ///
    /// # Errors
    ///
    /// Guaranteed-final-entry attachments reject this operation. Teardown also
    /// rejects outstanding entries or a live cleanup guard.
    pub fn detach_without_context(&mut self) -> Result<DetachReport, RuntimeError> {
        self.shared.ensure_thread()?;
        if self.shared.gate.state() == HostState::Destroyed {
            return Ok(self.empty_report());
        }

        self.shared.gate.request_drain();
        let final_entry = self
            .shared
            .gate
            .finish_drain()
            .map_err(RuntimeError::Host)?;
        let (unresolved_persistent_roots, unresolved_host_functions, retired_native_states) =
            self.abandon_rust_state();
        self.shared
            .gate
            .mark_destroyed()
            .map_err(RuntimeError::Host)?;

        Ok(self.report(
            final_entry,
            0,
            unresolved_persistent_roots,
            0,
            unresolved_host_functions,
            retired_native_states,
        ))
    }

    /// Returns the identity of this host-owned engine attachment.
    #[must_use]
    pub fn attachment_id(&self) -> AttachmentId {
        self.shared.id
    }

    /// Returns the attachment's monotonic lifecycle state.
    #[must_use]
    pub fn state(&self) -> HostState {
        self.shared.gate.state()
    }

    fn empty_report(&self) -> DetachReport {
        self.report(
            self.shared
                .gate
                .final_entry_outcome()
                .unwrap_or(FinalEntryOutcome::Unavailable),
            0,
            0,
            0,
            0,
            0,
        )
    }

    fn report(
        &self,
        final_entry: FinalEntryOutcome,
        released_persistent_roots: usize,
        unresolved_persistent_roots: usize,
        released_host_functions: usize,
        unresolved_host_functions: usize,
        retired_native_states: usize,
    ) -> DetachReport {
        DetachReport {
            final_entry,
            released_persistent_roots,
            unresolved_persistent_roots,
            released_host_functions,
            unresolved_host_functions,
            retired_native_states,
            remaining_external_allocations: self.shared.external_buffers.live_allocations(),
            remaining_external_bytes: self.shared.external_buffers.live_bytes(),
            callback_drop_panics: self.shared.callback_drop_panics.get(),
            native_state_drop_panics: self.shared.native_drop_panics.get(),
        }
    }

    fn retire_native_states(&self) -> usize {
        let states = self.shared.native_states.borrow_mut().drain();
        let count = states.len();
        super::native_state::drop_states(&self.shared, states);
        count
    }

    fn abandon_rust_state(&self) -> (usize, usize, usize) {
        let roots = self.shared.roots.borrow_mut().drain();
        let functions = std::mem::take(&mut *self.shared.host_functions.borrow_mut());
        let root_count = roots.len();
        let function_count = functions.len();
        drop(roots);
        for entry in functions.into_values() {
            self.shared.drop_callback(entry);
        }
        self.shared.close_native_finalizers();
        (root_count, function_count, self.retire_native_states())
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        if self.shared.gate.state() == HostState::Destroyed {
            return;
        }
        self.shared.gate.request_drain();
        if self.shared.gate.finish_drain().is_ok() {
            let _ = self.abandon_rust_state();
            let _ = self.shared.gate.mark_destroyed();
            return;
        }

        // A Guaranteed attachment dropped without final entry has violated its
        // host contract. Release Rust payloads safely, but never touch JSC here.
        let _ = self.abandon_rust_state();
    }
}

impl DetachReport {
    /// Returns whether host-authorized final engine cleanup completed.
    #[must_use]
    pub const fn final_entry(&self) -> FinalEntryOutcome {
        self.final_entry
    }

    /// Returns persistent roots successfully unprotected during final entry.
    #[must_use]
    pub const fn released_persistent_roots(&self) -> usize {
        self.released_persistent_roots
    }

    /// Returns persistent protections left for context destruction to reclaim.
    #[must_use]
    pub const fn unresolved_persistent_roots(&self) -> usize {
        self.unresolved_persistent_roots
    }

    /// Returns host-function roots successfully unprotected during final entry.
    #[must_use]
    pub const fn released_host_functions(&self) -> usize {
        self.released_host_functions
    }

    /// Returns host-function protections left for context destruction to reclaim.
    #[must_use]
    pub const fn unresolved_host_functions(&self) -> usize {
        self.unresolved_host_functions
    }

    /// Returns native Rust state payloads retired during detach.
    #[must_use]
    pub const fn retired_native_states(&self) -> usize {
        self.retired_native_states
    }

    /// Returns JSC-owned external allocations still live after detach.
    #[must_use]
    pub const fn remaining_external_allocations(&self) -> usize {
        self.remaining_external_allocations
    }

    /// Returns bytes in JSC-owned external allocations still live after detach.
    #[must_use]
    pub const fn remaining_external_bytes(&self) -> usize {
        self.remaining_external_bytes
    }

    /// Returns callback-capture destructor panics contained during this attachment.
    #[must_use]
    pub const fn callback_drop_panics(&self) -> usize {
        self.callback_drop_panics
    }

    /// Returns native-state destructor panics contained during this attachment.
    #[must_use]
    pub const fn native_state_drop_panics(&self) -> usize {
        self.native_state_drop_panics
    }
}

pub(super) fn borrowed_global_context(
    context: *mut c_void,
) -> Result<NonNull<sys::OpaqueContext>, RuntimeError> {
    NonNull::new(context.cast::<sys::OpaqueContext>()).ok_or(RuntimeError::NullContext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Value, sys};

    struct ForeignContext(NonNull<sys::OpaqueContext>);

    impl ForeignContext {
        fn new() -> Self {
            // SAFETY: A null class requests JSC's default global object class.
            let context = unsafe { sys::global_context_create(std::ptr::null_mut()) };
            Self(NonNull::new(context).expect("JSC test context"))
        }

        fn as_raw(&self) -> *mut c_void {
            self.0.as_ptr().cast()
        }
    }

    impl Drop for ForeignContext {
        fn drop(&mut self) {
            // SAFETY: This helper owns the context and releases it exactly once.
            unsafe { sys::global_context_release(self.0.as_ptr()) };
        }
    }

    struct PanicDrop;

    impl Drop for PanicDrop {
        fn drop(&mut self) {
            panic!("attachment payload destructor");
        }
    }

    #[test]
    fn final_entry_detaches_without_releasing_the_foreign_context() {
        let owner = ForeignContext::new();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut first = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
        let first_id = first.attachment_id();
        let handles = unsafe {
            first.with_context(owner.as_raw(), |cx| {
                let local = cx.eval("({ answer: 42 })", "foreign.js").unwrap();
                let root = cx.persist(&local).unwrap();
                let function = cx
                    .install_host_function("foreignAnswer", |_| Ok(Value::Number(42.0)))
                    .unwrap();
                let native = cx
                    .install_native_state("foreignState", String::from("state"))
                    .unwrap();
                (root, function, native)
            })
        }
        .unwrap();

        let report = unsafe { first.detach_with_context(owner.as_raw()) }.unwrap();
        assert_eq!(report.final_entry(), FinalEntryOutcome::Completed);
        assert_eq!(report.released_persistent_roots(), 1);
        assert_eq!(report.released_host_functions(), 1);
        assert_eq!(report.unresolved_persistent_roots(), 0);
        assert_eq!(report.unresolved_host_functions(), 0);
        assert_eq!(report.retired_native_states(), 1);
        assert_eq!(first.state(), HostState::Destroyed);
        drop(handles);

        let mut replacement = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
        assert_eq!(
            replacement.attachment_id().runtime_id(),
            first_id.runtime_id()
        );
        assert_ne!(replacement.attachment_id().epoch(), first_id.epoch());
        unsafe {
            replacement.with_context(owner.as_raw(), |cx| {
                let value = cx.eval("40 + 2", "replacement.js").unwrap();
                assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
            })
        }
        .unwrap();
        let _ = unsafe { replacement.detach_with_context(owner.as_raw()) }.unwrap();
    }

    #[test]
    fn unavailable_final_entry_reports_unresolved_engine_roots() {
        let owner = ForeignContext::new();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Unavailable).unwrap();
        let handles = unsafe {
            attachment.with_context(owner.as_raw(), |cx| {
                let local = cx.eval("({})", "unresolved.js").unwrap();
                let root = cx.persist(&local).unwrap();
                let function = cx
                    .install_host_function("unresolved", |_| Ok(Value::Undefined))
                    .unwrap();
                let native = cx.install_native_state("native", 42_u64).unwrap();
                (root, function, native)
            })
        }
        .unwrap();

        let report = attachment.detach_without_context().unwrap();
        assert_eq!(report.final_entry(), FinalEntryOutcome::Unavailable);
        assert_eq!(report.released_persistent_roots(), 0);
        assert_eq!(report.released_host_functions(), 0);
        assert_eq!(report.unresolved_persistent_roots(), 1);
        assert_eq!(report.unresolved_host_functions(), 1);
        assert_eq!(report.retired_native_states(), 1);
        assert_eq!(attachment.state(), HostState::Destroyed);
        drop(handles);
    }

    #[test]
    fn external_owner_outlives_detach_and_reconciles_at_context_destruction() {
        let owner = ForeignContext::new();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Unavailable).unwrap();
        let buffer = unsafe {
            attachment.with_context(owner.as_raw(), |cx| {
                cx.install_external_buffer("foreignBytes", vec![1, 2, 3, 4].into_boxed_slice())
                    .unwrap()
            })
        }
        .unwrap();

        let report = attachment.detach_without_context().unwrap();
        assert_eq!(report.remaining_external_allocations(), 1);
        assert_eq!(report.remaining_external_bytes(), 4);
        assert!(!buffer.is_deallocated());
        drop(owner);
        assert!(buffer.is_deallocated());
        assert_eq!(buffer.deallocator_received_origin(), Some(true));
    }

    #[test]
    fn detach_report_exposes_contained_payload_destructor_panics() {
        let owner = ForeignContext::new();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
        let handles = unsafe {
            attachment.with_context(owner.as_raw(), |cx| {
                let callback_payload = PanicDrop;
                let function = cx
                    .install_host_function("panicDrop", move |_| {
                        let _ = &callback_payload;
                        Ok(Value::Undefined)
                    })
                    .unwrap();
                let native = cx.install_native_state("panicState", PanicDrop).unwrap();
                (function, native)
            })
        }
        .unwrap();

        let report = unsafe { attachment.detach_with_context(owner.as_raw()) }.unwrap();
        assert_eq!(report.callback_drop_panics(), 1);
        assert_eq!(report.native_state_drop_panics(), 1);
        drop(handles);
    }

    #[test]
    fn guaranteed_policy_can_retry_after_missing_or_null_cleanup_context() {
        let owner = ForeignContext::new();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();

        assert_eq!(
            attachment.detach_without_context(),
            Err(RuntimeError::Host(
                rustjsi_host::GateError::FinalEntryRequired
            ))
        );
        assert_eq!(attachment.state(), HostState::Draining);
        assert_eq!(
            unsafe { attachment.detach_with_context(std::ptr::null_mut()) },
            Err(RuntimeError::NullContext)
        );
        assert_eq!(attachment.state(), HostState::Draining);
        assert_eq!(
            unsafe { attachment.detach_with_context(owner.as_raw()) }
                .unwrap()
                .final_entry(),
            FinalEntryOutcome::Completed
        );
    }

    #[test]
    fn null_normal_entry_is_rejected_without_changing_lifecycle() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap();
        assert_eq!(
            unsafe { attachment.with_context(std::ptr::null_mut(), |_| ()) },
            Err(RuntimeError::NullContext)
        );
        assert_eq!(attachment.state(), HostState::Active);
        let _ = attachment.detach_without_context().unwrap();
    }

    #[test]
    fn dropping_empty_attachment_does_not_destroy_the_foreign_context() {
        let owner = ForeignContext::new();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        drop(Attachment::new(&mut identity, FinalEntryPolicy::BestEffort).unwrap());

        let mut replacement = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
        unsafe {
            replacement.with_context(owner.as_raw(), |cx| {
                let value = cx.eval("6 * 7", "after-drop.js").unwrap();
                assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
            })
        }
        .unwrap();
        let _ = unsafe { replacement.detach_with_context(owner.as_raw()) }.unwrap();
    }
}
