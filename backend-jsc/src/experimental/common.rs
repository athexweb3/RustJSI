// SPDX-License-Identifier: MIT OR Apache-2.0

//! Common `RustJSI` backend contract over one host-authorized JSC entry.

use super::external_buffer::{make_external_object, new_observation};
use super::local_budget::Reservation;
use super::local_roots::LocalRoots;
use super::{
    ActiveRuntimeGuard, Attachment, JsError, JsException, JsString, RootId, Runtime, RuntimeError,
    Shared, exception_to_owned, value_to_string,
};
use crate::sys;
use rustjsi_backend::{
    BACKEND_CONTRACT_VERSION, BackendBase, BackendError, BackendException, BackendFamily,
    BackendManifest, BackendScope, Capability, CapabilitySet, OwnedExternalBufferScope,
    OwnershipTransferError, RootBackend, RootScope, ValueKind,
};
use rustjsi_host::AttachmentId;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::rc::Rc;

/// Type family for host-authorized JSC entries and scopes.
///
/// External backing does not imply stable borrowed-byte access:
///
/// ```compile_fail
/// use rustjsi_backend::{BackendFamily, BorrowedBufferScope};
/// use rustjsi_backend_jsc::JscBackendFamily;
/// fn require_bytes<F: BackendFamily>()
/// where for<'scope> F::Scope<'scope>: BorrowedBufferScope {}
/// require_bytes::<JscBackendFamily>();
/// ```
#[derive(Debug)]
pub enum JscBackendFamily {}

impl BackendFamily for JscBackendFamily {
    type Backend<'entry> = JscBackend<'entry>;
    type Scope<'scope> = JscScope<'scope, 'scope>;

    fn try_with_scope<R>(
        backend: &mut JscBackend<'_>,
        operation: impl for<'scope> FnOnce(Self::Scope<'scope>) -> Result<R, BackendError>,
    ) -> Result<R, BackendError> {
        operation(backend.open_scope()?)
    }
}

/// Engine mechanics exposed only during a host-authorized JSC entry.
///
/// `JscBackend` does not own runtime lifecycle or scheduling. The experimental
/// standalone [`Runtime`] acts as the host in this spike and lends this adapter
/// through [`Runtime::with_backend`].
///
/// ```compile_fail
/// use rustjsi_backend_jsc::JscBackend;
/// fn require_send<T: Send>() {}
/// require_send::<JscBackend<'static>>();
/// ```
pub struct JscBackend<'entry> {
    shared: &'entry Rc<Shared>,
    raw: NonNull<sys::OpaqueContext>,
    _affine: PhantomData<Rc<()>>,
}

/// A local-handle scope inside one authorized JSC entry.
pub struct JscScope<'scope, 'entry> {
    backend: &'scope JscBackend<'entry>,
    roots: RefCell<LocalRoots>,
}

/// A JSC value kept valid for one [`JscScope`].
///
/// ```compile_fail
/// use rustjsi_backend::{BackendBase, BackendScope};
/// use rustjsi_backend_jsc::Runtime;
///
/// let mut runtime = Runtime::new().unwrap();
/// let value = runtime
///     .with_backend(|backend| {
///         let scope = backend.open_scope().unwrap();
///         scope.number(42.0).unwrap()
///     })
///     .unwrap();
/// drop(value);
/// ```
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct JscValue<'scope> {
    attachment: AttachmentId,
    raw: NonNull<sys::OpaqueValue>,
    _scope: PhantomData<&'scope ()>,
    _affine: PhantomData<Rc<()>>,
}

/// An explicit, generational JSC strong root that can cross scope entries.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct JscRoot {
    attachment: AttachmentId,
    id: RootId,
    _affine: PhantomData<Rc<()>>,
}

impl Runtime {
    /// Lends engine mechanics for one host-authorized runtime entry.
    ///
    /// The adapter and all scoped values are unable to escape `operation`.
    ///
    /// # Errors
    ///
    /// Returns an affinity or lifecycle error when host entry is not legal.
    pub fn with_backend<R>(
        &mut self,
        operation: impl for<'entry> FnOnce(&mut JscBackend<'entry>) -> R,
    ) -> Result<R, RuntimeError> {
        self.shared.ensure_active()?;
        let _entry = self.shared.gate.try_enter().map_err(RuntimeError::Host)?;
        let raw = self.context.ok_or(RuntimeError::Invalidated)?;
        let active = ActiveRuntimeGuard::enter(Rc::as_ptr(&self.shared), raw);
        self.shared.drain_native_finalizers();
        self.shared.drain_root_releases(raw);
        let result = {
            let mut backend = JscBackend {
                shared: &self.shared,
                raw,
                _affine: PhantomData,
            };
            operation(&mut backend)
        };
        self.shared.drain_native_finalizers();
        self.shared.drain_root_releases(raw);
        drop(active);
        Ok(result)
    }
}

impl Attachment {
    /// Lends common JSC backend mechanics for one host-authorized entry.
    ///
    /// # Safety
    ///
    /// `context` must satisfy the lifetime, identity, thread, and synchronization
    /// requirements documented by [`Attachment::with_context`].
    ///
    /// # Errors
    ///
    /// Returns an affinity, lifecycle, admission, or null-context error.
    pub unsafe fn with_backend<R>(
        &mut self,
        context: *mut std::ffi::c_void,
        operation: impl for<'entry> FnOnce(&mut JscBackend<'entry>) -> R,
    ) -> Result<R, RuntimeError> {
        self.shared.ensure_active()?;
        let raw = super::attachment::borrowed_global_context(context)?;
        let _entry = self.shared.gate.try_enter().map_err(RuntimeError::Host)?;
        let active = ActiveRuntimeGuard::enter(Rc::as_ptr(&self.shared), raw);
        self.shared.drain_native_finalizers();
        self.shared.drain_root_releases(raw);
        let result = {
            let mut backend = JscBackend {
                shared: &self.shared,
                raw,
                _affine: PhantomData,
            };
            operation(&mut backend)
        };
        self.shared.drain_native_finalizers();
        self.shared.drain_root_releases(raw);
        drop(active);
        Ok(result)
    }
}

impl<'entry> BackendBase for JscBackend<'entry> {
    type Scope<'scope>
        = JscScope<'scope, 'entry>
    where
        Self: 'scope;

    fn manifest(&self) -> BackendManifest {
        BackendManifest::new(
            BACKEND_CONTRACT_VERSION,
            CapabilitySet::only(Capability::StrongRoots).with(Capability::OwnedExternalBuffers),
        )
    }

    fn open_scope(&mut self) -> Result<Self::Scope<'_>, BackendError> {
        self.shared.ensure_active().map_err(map_runtime_error)?;
        Ok(JscScope {
            backend: self,
            roots: RefCell::new(LocalRoots::new()),
        })
    }
}

impl RootBackend for JscBackend<'_> {
    type Root = JscRoot;
}

impl<'entry> BackendScope for JscScope<'_, 'entry> {
    type Backend = JscBackend<'entry>;

    type Value<'value>
        = JscValue<'value>
    where
        Self: 'value;

    fn undefined(&self) -> Result<Self::Value<'_>, BackendError> {
        // SAFETY: The host-authorized context is active for this scope.
        self.primitive(unsafe { sys::value_make_undefined(self.backend.raw.as_ptr()) })
    }

    fn null(&self) -> Result<Self::Value<'_>, BackendError> {
        // SAFETY: The host-authorized context is active for this scope.
        self.primitive(unsafe { sys::value_make_null(self.backend.raw.as_ptr()) })
    }

    fn boolean(&self, value: bool) -> Result<Self::Value<'_>, BackendError> {
        // SAFETY: The host-authorized context is active for this scope.
        self.primitive(unsafe { sys::value_make_boolean(self.backend.raw.as_ptr(), value) })
    }

    fn number(&self, value: f64) -> Result<Self::Value<'_>, BackendError> {
        // SAFETY: The host-authorized context is active for this scope.
        self.primitive(unsafe { sys::value_make_number(self.backend.raw.as_ptr(), value) })
    }

    fn string(&self, value: &str) -> Result<Self::Value<'_>, BackendError> {
        let reservation = self
            .backend
            .shared
            .local_budget
            .reserve()
            .map_err(map_runtime_error)?;
        let string = JsString::new(value).map_err(map_js_error)?;
        // SAFETY: JSC retains the string contents in the created value.
        let raw = unsafe { sys::value_make_string(self.backend.raw.as_ptr(), string.as_ptr()) };
        self.rooted(raw, reservation)
    }

    fn evaluate(&self, source: &str, source_url: &str) -> Result<Self::Value<'_>, BackendError> {
        let reservation = self
            .backend
            .shared
            .local_budget
            .reserve()
            .map_err(map_runtime_error)?;
        let script = JsString::new(source).map_err(map_js_error)?;
        let url = JsString::new(source_url).map_err(map_js_error)?;
        let mut exception = ptr::null();
        // SAFETY: Strings and context remain live through this synchronous call.
        let raw = unsafe {
            sys::evaluate_script(
                self.backend.raw.as_ptr(),
                script.as_ptr(),
                ptr::null_mut(),
                url.as_ptr(),
                1,
                &raw mut exception,
            )
        };
        if !exception.is_null() {
            return Err(BackendError::Exception(
                exception_to_owned(self.backend.raw, exception).into(),
            ));
        }
        self.rooted(raw, reservation)
    }

    fn kind<'value>(&'value self, value: Self::Value<'value>) -> Result<ValueKind, BackendError> {
        self.ensure_value(value)?;
        self.raw_kind(value.raw)
    }

    fn as_boolean<'value>(&'value self, value: Self::Value<'value>) -> Result<bool, BackendError> {
        self.ensure_value(value)?;
        // SAFETY: Both handles belong to this active scope.
        if !unsafe { sys::value_is_boolean(self.backend.raw.as_ptr(), value.raw.as_ptr()) } {
            return Err(BackendError::Type {
                expected: ValueKind::Boolean,
                actual: self.raw_kind(value.raw)?,
            });
        }
        // SAFETY: The strict predicate above avoids coercion.
        Ok(unsafe { sys::value_to_boolean(self.backend.raw.as_ptr(), value.raw.as_ptr()) })
    }

    fn as_number<'value>(&'value self, value: Self::Value<'value>) -> Result<f64, BackendError> {
        self.ensure_value(value)?;
        // SAFETY: Both handles belong to this active scope.
        if !unsafe { sys::value_is_number(self.backend.raw.as_ptr(), value.raw.as_ptr()) } {
            return Err(BackendError::Type {
                expected: ValueKind::Number,
                actual: self.raw_kind(value.raw)?,
            });
        }
        let mut exception = ptr::null();
        // SAFETY: The strict predicate avoids coercion; exception is captured.
        let number = unsafe {
            sys::value_to_number(
                self.backend.raw.as_ptr(),
                value.raw.as_ptr(),
                &raw mut exception,
            )
        };
        if exception.is_null() {
            Ok(number)
        } else {
            Err(BackendError::Exception(
                exception_to_owned(self.backend.raw, exception).into(),
            ))
        }
    }

    fn to_string<'value>(&'value self, value: Self::Value<'value>) -> Result<String, BackendError> {
        self.ensure_value(value)?;
        // SAFETY: Both handles belong to this active scope.
        if !unsafe { sys::value_is_string(self.backend.raw.as_ptr(), value.raw.as_ptr()) } {
            return Err(BackendError::Type {
                expected: ValueKind::String,
                actual: self.raw_kind(value.raw)?,
            });
        }
        value_to_string(self.backend.raw, value.raw.as_ptr()).map_err(map_js_error)
    }
}

impl RootScope for JscScope<'_, '_> {
    fn persist<'value>(&'value self, value: Self::Value<'value>) -> Result<JscRoot, BackendError> {
        self.ensure_value(value)?;
        let id = self
            .backend
            .shared
            .roots
            .borrow_mut()
            .insert(value.raw)
            .map_err(map_runtime_error)?;
        // SAFETY: The registry owns one matching protection until release or runtime
        // invalidation.
        unsafe { sys::value_protect(self.backend.raw.as_ptr(), value.raw.as_ptr()) };
        Ok(JscRoot {
            attachment: self.backend.shared.id,
            id,
            _affine: PhantomData,
        })
    }

    fn resolve(&self, root: JscRoot) -> Result<Self::Value<'_>, BackendError> {
        self.ensure_root(root)?;
        let value = self
            .backend
            .shared
            .roots
            .borrow()
            .get(root.id)
            .ok_or(BackendError::StaleHandle)?;
        let reservation = self
            .backend
            .shared
            .local_budget
            .reserve()
            .map_err(map_runtime_error)?;
        Ok(self.root_nonnull(value, reservation))
    }

    fn release(&self, root: JscRoot) -> Result<(), BackendError> {
        self.ensure_root(root)?;
        let value = self
            .backend
            .shared
            .roots
            .borrow_mut()
            .remove(root.id)
            .ok_or(BackendError::StaleHandle)?;
        // SAFETY: The matching generational registry entry was removed exactly once.
        unsafe { sys::value_unprotect(self.backend.raw.as_ptr(), value.as_ptr()) };
        Ok(())
    }
}

impl OwnedExternalBufferScope for JscScope<'_, '_> {
    fn externalize(
        &self,
        owner: Box<[u8]>,
    ) -> Result<Self::Value<'_>, OwnershipTransferError<Box<[u8]>>> {
        let reservation = match self.backend.shared.local_budget.reserve() {
            Ok(reservation) => reservation,
            Err(error) => {
                return Err(OwnershipTransferError::Rejected {
                    error: map_runtime_error(error),
                    owner,
                });
            }
        };
        if let Err(error) = self.backend.shared.external_buffers.reserve(owner.len()) {
            return Err(OwnershipTransferError::Rejected {
                error: map_js_error(error),
                owner,
            });
        }

        let observation = new_observation();
        let object = match make_external_object(
            self.backend.shared,
            self.backend.raw,
            owner,
            &observation,
        ) {
            Ok((object, _)) => object,
            Err(error) => {
                return Err(OwnershipTransferError::Accepted {
                    error: map_js_error(error),
                });
            }
        };
        Ok(self.root_nonnull(object, reservation))
    }
}

impl Drop for JscScope<'_, '_> {
    fn drop(&mut self) {
        for value in self.roots.get_mut().drain() {
            // SAFETY: Each local root was protected once by this live scope.
            unsafe { sys::value_unprotect(self.backend.raw.as_ptr(), value.as_ptr()) };
            self.backend.shared.local_budget.release();
        }
    }
}

impl JscScope<'_, '_> {
    fn primitive(&self, raw: sys::ValueRef) -> Result<JscValue<'_>, BackendError> {
        let raw = NonNull::new(raw.cast_mut()).ok_or(BackendError::Failure(
            "JavaScriptCore returned a null primitive",
        ))?;
        Ok(self.value(raw))
    }

    fn rooted(
        &self,
        raw: sys::ValueRef,
        reservation: Reservation<'_>,
    ) -> Result<JscValue<'_>, BackendError> {
        let raw = NonNull::new(raw.cast_mut()).ok_or(BackendError::Failure(
            "JavaScriptCore returned a null value",
        ))?;
        Ok(self.root_nonnull(raw, reservation))
    }

    fn root_nonnull(
        &self,
        raw: NonNull<sys::OpaqueValue>,
        reservation: Reservation<'_>,
    ) -> JscValue<'_> {
        self.roots.borrow_mut().push(raw);
        // SAFETY: The value and context belong to this authorized entry. The scope
        // balances this protection in `Drop`.
        unsafe { sys::value_protect(self.backend.raw.as_ptr(), raw.as_ptr()) };
        reservation.commit();
        self.value(raw)
    }

    fn value(&self, raw: NonNull<sys::OpaqueValue>) -> JscValue<'_> {
        JscValue {
            attachment: self.backend.shared.id,
            raw,
            _scope: PhantomData,
            _affine: PhantomData,
        }
    }

    fn ensure_value(&self, value: JscValue<'_>) -> Result<(), BackendError> {
        if value.attachment == self.backend.shared.id {
            Ok(())
        } else {
            Err(BackendError::WrongBackend)
        }
    }

    fn ensure_root(&self, root: JscRoot) -> Result<(), BackendError> {
        if root.attachment == self.backend.shared.id {
            Ok(())
        } else {
            Err(BackendError::WrongBackend)
        }
    }

    fn raw_kind(&self, value: NonNull<sys::OpaqueValue>) -> Result<ValueKind, BackendError> {
        // SAFETY: The value is protected by this scope or is a non-GC primitive.
        let raw_type = unsafe { sys::value_get_type(self.backend.raw.as_ptr(), value.as_ptr()) };
        match raw_type {
            sys::TYPE_UNDEFINED => Ok(ValueKind::Undefined),
            sys::TYPE_NULL => Ok(ValueKind::Null),
            sys::TYPE_BOOLEAN => Ok(ValueKind::Boolean),
            sys::TYPE_NUMBER => Ok(ValueKind::Number),
            sys::TYPE_STRING => Ok(ValueKind::String),
            sys::TYPE_SYMBOL => Ok(ValueKind::Symbol),
            sys::TYPE_BIG_INT => Ok(ValueKind::BigInt),
            sys::TYPE_OBJECT => self.object_kind(value),
            _ => Err(BackendError::Failure(
                "JavaScriptCore returned an unknown value type",
            )),
        }
    }

    fn object_kind(&self, value: NonNull<sys::OpaqueValue>) -> Result<ValueKind, BackendError> {
        // SAFETY: JSC type classification proved this value is an object.
        if unsafe { sys::object_is_function(self.backend.raw.as_ptr(), value.as_ptr()) } {
            return Ok(ValueKind::Function);
        }
        let mut exception = ptr::null();
        // SAFETY: Both handles are live and the exception output is captured.
        let typed_array = unsafe {
            sys::value_get_typed_array_type(
                self.backend.raw.as_ptr(),
                value.as_ptr(),
                &raw mut exception,
            )
        };
        if !exception.is_null() {
            return Err(BackendError::Exception(
                exception_to_owned(self.backend.raw, exception).into(),
            ));
        }
        if typed_array == sys::TYPED_ARRAY_NONE {
            Ok(ValueKind::Object)
        } else {
            Ok(ValueKind::Buffer)
        }
    }
}

impl std::fmt::Debug for JscBackend<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JscBackend(..)")
    }
}

impl std::fmt::Debug for JscScope<'_, '_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JscScope(..)")
    }
}

impl std::fmt::Debug for JscValue<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JscValue(..)")
    }
}

impl std::fmt::Debug for JscRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JscRoot(..)")
    }
}

fn map_runtime_error(error: RuntimeError) -> BackendError {
    match error {
        RuntimeError::WrongRuntime => BackendError::WrongBackend,
        RuntimeError::StaleHandle => BackendError::StaleHandle,
        RuntimeError::Host(_) => BackendError::Failure("JavaScriptCore host entry rejected"),
        RuntimeError::CreationFailed => BackendError::Failure("JavaScriptCore creation failed"),
        RuntimeError::NullContext => BackendError::Failure("JavaScriptCore context is null"),
        RuntimeError::Invalidated => BackendError::Failure("JavaScriptCore runtime is invalid"),
        RuntimeError::WrongThread => {
            BackendError::Failure("JavaScriptCore runtime thread mismatch")
        }
        RuntimeError::IdentityExhausted => BackendError::Failure("runtime identity exhausted"),
        RuntimeError::ScopeDepthExceeded => BackendError::Failure("Context scope depth exceeded"),
        RuntimeError::PersistentRootLimitReached => {
            BackendError::Failure("persistent root slot limit reached")
        }
        RuntimeError::LocalRootLimitReached => {
            BackendError::Failure("local result root limit reached")
        }
        RuntimeError::HostFunctionLimitReached => {
            BackendError::Failure("host function registration limit reached")
        }
    }
}

impl From<JsException> for BackendException {
    fn from(error: JsException) -> Self {
        if error.truncated {
            Self::new_truncated(error.message)
        } else {
            Self::new(error.message)
        }
    }
}

fn map_js_error(error: JsError) -> BackendError {
    match error {
        JsError::Runtime(error) => map_runtime_error(error),
        JsError::Exception(error) => BackendError::Exception(error.into()),
        JsError::Type { .. } => BackendError::Failure("JavaScriptCore type mismatch"),
        JsError::Backend(message) => BackendError::Failure(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustjsi_backend::{BackendBase, BackendScope, RootScope};
    use rustjsi_host::{FinalEntryOutcome, FinalEntryPolicy, RuntimeIdentity};
    use rustjsi_testkit::{
        create_number_root, verify_base_values, verify_number_root_and_release,
        verify_owned_external_buffer,
    };

    #[test]
    fn common_base_and_roots_pass_shared_conformance() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_backend(|backend| {
                verify_base_values(backend).unwrap();
                let root = {
                    let scope = backend.open_scope().unwrap();
                    create_number_root(&scope).unwrap()
                };
                let scope = backend.open_scope().unwrap();
                verify_number_root_and_release(&scope, root).unwrap();
            })
            .unwrap();
    }

    #[test]
    fn common_roots_share_the_configured_registry_limit() {
        let mut runtime = Runtime::new_with_persistent_root_limit(1).unwrap();
        runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let value = scope.evaluate("({})", "root-budget.js").unwrap();
                let root = scope.persist(value).unwrap();
                let second = scope.evaluate("({})", "root-budget.js").unwrap();
                assert_eq!(
                    scope.persist(second),
                    Err(BackendError::Failure("persistent root slot limit reached"))
                );
                scope.release(root).unwrap();
                scope.persist(second).unwrap();
            })
            .unwrap();
    }

    #[test]
    fn semantic_value_kinds_include_jsc_refinements() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let symbol = scope.evaluate("Symbol('rustjsi')", "kind-test.js").unwrap();
                assert_eq!(scope.kind(symbol), Ok(ValueKind::Symbol));
                let bigint = scope.evaluate("1n", "kind-test.js").unwrap();
                assert_eq!(scope.kind(bigint), Ok(ValueKind::BigInt));
                let function = scope.evaluate("(() => 1)", "kind-test.js").unwrap();
                assert_eq!(scope.kind(function), Ok(ValueKind::Function));
                let object = scope.evaluate("({})", "kind-test.js").unwrap();
                assert_eq!(scope.kind(object), Ok(ValueKind::Object));
                let typed = scope.evaluate("new Uint8Array(4)", "kind-test.js").unwrap();
                assert_eq!(scope.kind(typed), Ok(ValueKind::Buffer));
            })
            .unwrap();
    }

    #[test]
    fn local_object_remains_rooted_after_moving_to_heap_and_gc() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let value = Box::new(scope.evaluate("({ answer: 42 })", "gc-test.js").unwrap());
                // SAFETY: The backend is in a host-authorized entry and the common
                // scope has protected the heap-stored value.
                unsafe { sys::garbage_collect(scope.backend.raw.as_ptr()) };
                assert_eq!(scope.kind(*value), Ok(ValueKind::Object));
            })
            .unwrap();
    }

    #[test]
    fn root_crosses_distinct_host_authorized_entries() {
        let mut runtime = Runtime::new().unwrap();
        let root = runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let value = scope.number(19.0).unwrap();
                scope.persist(value).unwrap()
            })
            .unwrap();

        runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let value = scope.resolve(root).unwrap();
                assert!((scope.as_number(value).unwrap() - 19.0).abs() < f64::EPSILON);
                scope.release(root).unwrap();
            })
            .unwrap();
    }

    #[test]
    fn external_ownership_does_not_advertise_borrowed_bytes() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_backend(|backend| {
                assert!(
                    backend
                        .manifest()
                        .capabilities()
                        .contains(Capability::OwnedExternalBuffers)
                );
                assert!(
                    !backend
                        .manifest()
                        .capabilities()
                        .contains(Capability::BorrowedBufferBytes)
                );
                let scope = backend.open_scope().unwrap();
                verify_owned_external_buffer(&scope).unwrap();
            })
            .unwrap();

        for _ in 0..32 {
            if runtime.shared.external_buffers.live_allocations() == 0 {
                break;
            }
            runtime
                .with_context(|context| {
                    context.collect_garbage().unwrap();
                    context
                        .eval(
                            "Array.from({ length: 4096 }, (_, i) => ({ i }))",
                            "common-external-gc.js",
                        )
                        .unwrap();
                })
                .unwrap();
        }
        assert_eq!(runtime.shared.external_buffers.live_allocations(), 0);
    }

    #[test]
    fn common_roots_reject_another_runtime() {
        let mut first = Runtime::new().unwrap();
        let root = first
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let value = scope.number(7.0).unwrap();
                scope.persist(value).unwrap()
            })
            .unwrap();

        let mut second = Runtime::new().unwrap();
        second
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                assert_eq!(scope.resolve(root), Err(BackendError::WrongBackend));
                assert_eq!(scope.release(root), Err(BackendError::WrongBackend));
            })
            .unwrap();

        first
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                scope.release(root).unwrap();
            })
            .unwrap();
    }

    #[test]
    fn foreign_attachment_lends_the_common_backend_without_owning_jsc() {
        let mut owner = Runtime::new().unwrap();
        let raw = owner.context.unwrap().as_ptr().cast();
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();

        let root = unsafe {
            attachment.with_backend(raw, |backend| {
                let scope = backend.open_scope().unwrap();
                let value = scope.evaluate("21 * 2", "foreign-common.js").unwrap();
                scope.persist(value).unwrap()
            })
        }
        .unwrap();
        unsafe {
            attachment.with_backend(raw, |backend| {
                let scope = backend.open_scope().unwrap();
                let value = scope.resolve(root).unwrap();
                assert_eq!(
                    scope.as_number(value).unwrap().to_bits(),
                    42.0_f64.to_bits()
                );
            })
        }
        .unwrap();

        let report = unsafe { attachment.detach_with_context(raw) }.unwrap();
        assert_eq!(report.final_entry(), FinalEntryOutcome::Completed);
        assert_eq!(report.released_persistent_roots(), 1);
        owner
            .with_context(|cx| {
                let value = cx.eval("40 + 2", "owner-still-live.js").unwrap();
                assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
            })
            .unwrap();
    }
}
