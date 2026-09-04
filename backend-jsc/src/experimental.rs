// SPDX-License-Identifier: MIT OR Apache-2.0

//! Experimental direct integration with the macOS `JavaScriptCore` C API.

use crate::sys;
use rustjsi_host::{EntryGate, GateError, HostState};
mod common;
mod external_buffer;
mod local_roots;
mod native_state;
mod panic_boundary;

use local_roots::LocalRoots;
use panic_boundary::contain_unwind;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ptr::{self, NonNull};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};

pub use common::{JscBackend, JscRoot, JscScope, JscValue};
pub use external_buffer::ExternalBuffer;
pub use native_state::NativeObject;

thread_local! {
    static ACTIVE_RUNTIME: Cell<*const Shared> = const { Cell::new(ptr::null()) };
}

const INLINE_ARGUMENTS: usize = 8;
const ENTRY_LIMIT: NonZeroU32 = NonZeroU32::new(64).unwrap();
static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// A standalone `JavaScriptCore` runtime owned by the current thread.
///
/// ```compile_fail
/// use rustjsi_backend_jsc::Runtime;
/// fn require_send<T: Send>() {}
/// require_send::<Runtime>();
/// ```
pub struct Runtime {
    shared: Rc<Shared>,
}

/// A scoped, legal entry into a runtime.
pub struct Context<'cx> {
    shared: &'cx Rc<Shared>,
    raw: NonNull<sys::OpaqueContext>,
    local_roots: RefCell<LocalRoots>,
    _affine: PhantomData<Rc<()>>,
}

/// A JavaScript value kept live until its context entry ends.
///
/// Managed values remain rooted even when this handle is moved to a Rust heap
/// container. Dropping the handle does not release its context-owned root.
///
/// ```compile_fail
/// use rustjsi_backend_jsc::Runtime;
/// let mut runtime = Runtime::new().unwrap();
/// let local = runtime.with_context(|cx| cx.eval("({})", "escape.js").unwrap()).unwrap();
/// drop(local);
/// ```
pub struct Local<'cx> {
    runtime: *const Shared,
    value: NonNull<sys::OpaqueValue>,
    _scope: PhantomData<&'cx Context<'cx>>,
    _affine: PhantomData<Rc<()>>,
}

/// A JavaScript value protected from collection until its last lease is dropped.
pub struct Persistent {
    lease: Rc<RootLease>,
}

/// An installed Rust host function.
pub struct HostFunction {
    runtime: Weak<Shared>,
    key: usize,
}

/// Borrowed arguments for one JavaScript-to-Rust call.
pub struct Call<'call> {
    raw_context: NonNull<sys::OpaqueContext>,
    arguments: &'call [sys::ValueRef],
    _affine: PhantomData<Rc<()>>,
}

/// An owned primitive crossing the Rust/JavaScript boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// JavaScript `undefined`.
    Undefined,
    /// JavaScript `null`.
    Null,
    /// A Boolean.
    Boolean(bool),
    /// A JavaScript number.
    Number(f64),
    /// A JavaScript string.
    String(String),
}

/// A failure returned by a Rust host function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostError {
    message: String,
}

/// An owned JavaScript exception.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsException {
    message: String,
}

/// A runtime lifecycle or affinity failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// `JavaScriptCore` could not create a context.
    CreationFailed,
    /// The runtime is no longer active.
    Invalidated,
    /// The operation was attempted from a different thread.
    WrongThread,
    /// A handle belongs to another runtime.
    WrongRuntime,
    /// A handle no longer names a protected value.
    StaleHandle,
    /// The process exhausted unique runtime identities.
    IdentityExhausted,
    /// Host entry accounting rejected an operation.
    Host(GateError),
}

/// A `JavaScriptCore` operation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsError {
    /// Runtime lifecycle or affinity failure.
    Runtime(RuntimeError),
    /// JavaScript threw an exception.
    Exception(JsException),
    /// A value did not have the required type.
    Type {
        /// Required JavaScript type.
        expected: &'static str,
    },
    /// `JavaScriptCore` returned an invalid result without an exception.
    Backend(&'static str),
}

type Callback = dyn for<'call> Fn(Call<'call>) -> Result<Value, HostError> + 'static;

struct Shared {
    id: u64,
    owner: ThreadId,
    gate: EntryGate,
    context: Cell<Option<NonNull<sys::OpaqueContext>>>,
    roots: RefCell<RootRegistry>,
    host_functions: RefCell<HashMap<usize, HostFunctionEntry>>,
    native_states: RefCell<native_state::NativeRegistry>,
    native_finalizers: Arc<native_state::FinalizerQueue>,
    native_drop_panics: Cell<usize>,
    callback_drop_panics: Cell<usize>,
    #[cfg(test)]
    context_local_roots: Cell<usize>,
    external_buffers: Arc<external_buffer::ExternalLedger>,
}

struct HostFunctionEntry {
    function: NonNull<sys::OpaqueValue>,
    callback: Rc<Callback>,
}

struct RootLease {
    runtime: Weak<Shared>,
    id: RootId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootId {
    slot: usize,
    generation: u64,
}

#[derive(Default)]
struct RootRegistry {
    slots: Vec<RootSlot>,
    free: Vec<usize>,
}

struct RootSlot {
    generation: u64,
    value: Option<NonNull<sys::OpaqueValue>>,
}

struct ActiveRuntimeGuard {
    previous: *const Shared,
}

struct JsString(NonNull<sys::OpaqueString>);

impl Runtime {
    /// Creates an isolated `JavaScriptCore` global context.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::CreationFailed`] if JSC cannot create the context.
    pub fn new() -> Result<Self, RuntimeError> {
        let id = NEXT_RUNTIME_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| RuntimeError::IdentityExhausted)?;
        // SAFETY: A null class requests JSC's default global object class. The returned
        // context is checked before ownership is placed in `Runtime`.
        let context = unsafe { sys::global_context_create(ptr::null_mut()) };
        let context = NonNull::new(context).ok_or(RuntimeError::CreationFailed)?;

        let native_finalizers = Arc::new(native_state::FinalizerQueue::new());
        Ok(Self {
            shared: Rc::new(Shared {
                id,
                owner: thread::current().id(),
                gate: EntryGate::new(ENTRY_LIMIT),
                context: Cell::new(Some(context)),
                roots: RefCell::new(RootRegistry::default()),
                host_functions: RefCell::new(HashMap::new()),
                native_states: RefCell::new(native_state::NativeRegistry::default()),
                native_finalizers,
                native_drop_panics: Cell::new(0),
                callback_drop_panics: Cell::new(0),
                #[cfg(test)]
                context_local_roots: Cell::new(0),
                external_buffers: Arc::new(external_buffer::ExternalLedger::new()),
            }),
        })
    }

    /// Enters the runtime for the duration of `operation`.
    ///
    /// # Errors
    ///
    /// Returns an affinity or lifecycle error when entry is not legal.
    pub fn with_context<R>(
        &mut self,
        operation: impl for<'cx> FnOnce(&mut Context<'cx>) -> R,
    ) -> Result<R, RuntimeError> {
        self.shared.ensure_active()?;
        let _entry = self.shared.gate.try_enter().map_err(RuntimeError::Host)?;
        self.shared.drain_native_finalizers();
        let context = self.shared.context.get().ok_or(RuntimeError::Invalidated)?;
        let active = ActiveRuntimeGuard::enter(Rc::as_ptr(&self.shared));
        let result = {
            let mut scoped = Context {
                shared: &self.shared,
                raw: context,
                local_roots: RefCell::new(LocalRoots::new()),
                _affine: PhantomData,
            };
            operation(&mut scoped)
        };
        drop(active);
        self.shared.drain_native_finalizers();
        Ok(result)
    }

    /// Invalidates handles, releases roots, and destroys the JSC context.
    ///
    /// # Errors
    ///
    /// Returns an affinity error or refuses teardown while entries remain.
    pub fn invalidate(&mut self) -> Result<(), RuntimeError> {
        self.shared.ensure_thread()?;
        if self.shared.gate.state() == HostState::Destroyed {
            return Ok(());
        }

        self.shared.gate.request_drain();
        if !self.shared.gate.is_drain_ready() {
            return Err(RuntimeError::Host(GateError::EntriesRemain(
                self.shared.gate.active_entries(),
            )));
        }
        let context = self.shared.context.get().ok_or(RuntimeError::Invalidated)?;
        let roots = self.shared.roots.borrow_mut().drain();
        let functions = std::mem::take(&mut *self.shared.host_functions.borrow_mut());

        for value in roots
            .into_iter()
            .chain(functions.values().map(|entry| entry.function))
        {
            // SAFETY: Every value was protected exactly once in this context and is
            // unprotected before the context is released, on its owning thread.
            unsafe { sys::value_unprotect(context.as_ptr(), value.as_ptr()) };
        }
        for entry in functions.into_values() {
            self.shared.drop_callback(entry);
        }
        self.shared.close_native_finalizers();
        self.shared
            .gate
            .finish_drain()
            .map_err(RuntimeError::Host)?;

        // SAFETY: `Runtime` owns the retained global context, all RustJSI roots have
        // been released, and the owning thread is performing the single release.
        unsafe { sys::global_context_release(context.as_ptr()) };
        self.shared.context.set(None);
        let native_states = self.shared.native_states.borrow_mut().drain();
        native_state::drop_states(&self.shared, native_states);
        self.shared
            .gate
            .mark_destroyed()
            .map_err(RuntimeError::Host)?;
        Ok(())
    }

    /// Returns the number of contained registered-callback destruction panics.
    ///
    /// The saturating counter remains readable after explicit invalidation.
    /// Abort-mode panics and double panics during unwinding are not recoverable.
    #[must_use]
    pub fn callback_drop_panics(&self) -> usize {
        self.shared.callback_drop_panics.get()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.invalidate();
    }
}

impl<'cx> Context<'cx> {
    /// Evaluates JavaScript and returns a scoped result.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, backend, or JavaScript exception error.
    pub fn eval(&mut self, source: &str, source_url: &str) -> Result<Local<'cx>, JsError> {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        let script = JsString::new(source)?;
        let url = JsString::new(source_url)?;
        let mut exception = ptr::null();

        // SAFETY: Both strings and the context remain live for this call. A null
        // `this` selects the global object; the result is checked with `exception`.
        let value = unsafe {
            sys::evaluate_script(
                self.raw.as_ptr(),
                script.as_ptr(),
                ptr::null_mut(),
                url.as_ptr(),
                1,
                &raw mut exception,
            )
        };
        self.local_or_exception(value, exception)
    }

    /// Protects a scoped value so it can survive later entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime is inactive or the value is foreign.
    pub fn persist(&mut self, local: &Local<'_>) -> Result<Persistent, JsError> {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        self.ensure_local_runtime(local)?;
        let id = self.shared.roots.borrow_mut().insert(local.value);

        // SAFETY: The local belongs to this context and the registry will balance this
        // protection on last lease drop or runtime invalidation.
        unsafe { sys::value_protect(self.raw.as_ptr(), local.value.as_ptr()) };

        Ok(Persistent {
            lease: Rc::new(RootLease {
                runtime: Rc::downgrade(self.shared),
                id,
            }),
        })
    }

    /// Resolves a persistent value inside this runtime entry.
    ///
    /// # Errors
    ///
    /// Returns an error for a dead, foreign, or stale handle.
    pub fn resolve(&mut self, persistent: &Persistent) -> Result<Local<'cx>, JsError> {
        let runtime = persistent
            .lease
            .runtime
            .upgrade()
            .ok_or(JsError::Runtime(RuntimeError::Invalidated))?;
        if !Rc::ptr_eq(&runtime, self.shared) {
            return Err(JsError::Runtime(RuntimeError::WrongRuntime));
        }
        let value = self
            .shared
            .roots
            .borrow()
            .get(persistent.lease.id)
            .ok_or(JsError::Runtime(RuntimeError::StaleHandle))?;
        Ok(self.root_local(value))
    }

    /// Installs a stateful Rust callback as a global JavaScript function.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, backend, or JavaScript publication error.
    pub fn install_host_function<F>(
        &mut self,
        name: &str,
        callback: F,
    ) -> Result<HostFunction, JsError>
    where
        F: for<'call> Fn(Call<'call>) -> Result<Value, HostError> + 'static,
    {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        let name = JsString::new(name)?;

        // SAFETY: The callback has the exact JSC C ABI and contains all unwinding.
        let function = unsafe {
            sys::object_make_function_with_callback(
                self.raw.as_ptr(),
                name.as_ptr(),
                Some(host_function_callback),
            )
        };
        let function = NonNull::new(function).ok_or(JsError::Backend(
            "JavaScriptCore returned a null host function",
        ))?;
        let key = function.as_ptr() as usize;
        self.shared.host_functions.borrow_mut().insert(
            key,
            HostFunctionEntry {
                function,
                callback: Rc::new(callback),
            },
        );

        // SAFETY: The registry balances this protection at invalidation, preventing
        // function-address reuse while the callback map contains the address.
        unsafe { sys::value_protect(self.raw.as_ptr(), function.as_ptr()) };

        // SAFETY: The active context always has a global object.
        let global = unsafe { sys::context_get_global_object(self.raw.as_ptr()) };
        let Some(global) = NonNull::new(global) else {
            self.rollback_host_function(key, function);
            return Err(JsError::Backend(
                "JavaScriptCore returned a null global object",
            ));
        };
        let mut exception = ptr::null();
        // SAFETY: The global object, name, and protected function belong to this
        // context. The exception output is initialized and checked below.
        unsafe {
            sys::object_set_property(
                self.raw.as_ptr(),
                global.as_ptr(),
                name.as_ptr(),
                function.as_ptr(),
                0,
                &raw mut exception,
            );
        }
        if !exception.is_null() {
            let error = JsError::Exception(exception_to_owned(self.raw, exception));
            self.rollback_host_function(key, function);
            return Err(error);
        }

        Ok(HostFunction {
            runtime: Rc::downgrade(self.shared),
            key,
        })
    }

    fn rollback_host_function(&self, key: usize, function: NonNull<sys::OpaqueValue>) {
        let removed = self.shared.host_functions.borrow_mut().remove(&key);
        // SAFETY: Publication failed after protection in this still-live context.
        // Retire the registration and release its root before running user Drop.
        unsafe { sys::value_unprotect(self.raw.as_ptr(), function.as_ptr()) };
        if let Some(entry) = removed {
            self.shared.drop_callback(entry);
        }
    }

    /// Calls an installed Rust host function through `JavaScriptCore`.
    ///
    /// # Errors
    ///
    /// Returns an error for a dead/foreign handle, conversion failure, or exception.
    pub fn call(
        &mut self,
        function: &HostFunction,
        arguments: &[Value],
    ) -> Result<Local<'cx>, JsError> {
        let runtime = function
            .runtime
            .upgrade()
            .ok_or(JsError::Runtime(RuntimeError::Invalidated))?;
        if !Rc::ptr_eq(&runtime, self.shared) {
            return Err(JsError::Runtime(RuntimeError::WrongRuntime));
        }
        let function = self
            .shared
            .host_functions
            .borrow()
            .get(&function.key)
            .map(|entry| entry.function)
            .ok_or(JsError::Runtime(RuntimeError::StaleHandle))?;

        let mut string_storage = Vec::new();
        let mut inline_arguments = [ptr::null(); INLINE_ARGUMENTS];
        let mut heap_arguments = Vec::new();
        let raw_arguments = if arguments.len() <= INLINE_ARGUMENTS {
            for (slot, value) in inline_arguments.iter_mut().zip(arguments) {
                *slot = value_to_raw(self.raw, value, &mut string_storage)?;
            }
            &inline_arguments[..arguments.len()]
        } else {
            heap_arguments.reserve(arguments.len());
            for value in arguments {
                heap_arguments.push(value_to_raw(self.raw, value, &mut string_storage)?);
            }
            &heap_arguments
        };
        let mut exception = ptr::null();
        let argument_pointer = if raw_arguments.is_empty() {
            ptr::null()
        } else {
            raw_arguments.as_ptr()
        };

        // SAFETY: The function is protected by this runtime. Argument values and their
        // backing JSC strings remain live through the synchronous call.
        let value = unsafe {
            sys::object_call_as_function(
                self.raw.as_ptr(),
                function.as_ptr(),
                ptr::null_mut(),
                raw_arguments.len(),
                argument_pointer,
                &raw mut exception,
            )
        };
        self.local_or_exception(value, exception)
    }

    /// Reads a JavaScript number without coercion.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is foreign or is not a number.
    pub fn number(&self, local: &Local<'_>) -> Result<f64, JsError> {
        self.ensure_local_runtime(local)?;
        strict_number(self.raw, local.value.as_ptr())
    }

    /// Reads a JavaScript Boolean without coercion.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is foreign or is not a Boolean.
    pub fn boolean(&self, local: &Local<'_>) -> Result<bool, JsError> {
        self.ensure_local_runtime(local)?;
        strict_boolean(self.raw, local.value.as_ptr())
    }

    /// Converts a JavaScript value to its string representation.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is foreign or conversion throws/fails.
    pub fn string(&self, local: &Local<'_>) -> Result<String, JsError> {
        self.ensure_local_runtime(local)?;
        value_to_string(self.raw, local.value.as_ptr())
    }

    /// Requests a synchronous garbage collection for lifecycle testing.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle or affinity error when the runtime is not active.
    pub fn collect_garbage(&mut self) -> Result<(), JsError> {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        // SAFETY: The context is active on its owning thread. JSC permits explicit
        // collection and preserves stack-visible or protected values.
        unsafe { sys::garbage_collect(self.raw.as_ptr()) };
        Ok(())
    }

    fn ensure_local_runtime(&self, local: &Local<'_>) -> Result<(), JsError> {
        if ptr::eq(local.runtime, Rc::as_ptr(self.shared)) {
            Ok(())
        } else {
            Err(JsError::Runtime(RuntimeError::WrongRuntime))
        }
    }

    fn local_or_exception(
        &self,
        value: sys::ValueRef,
        exception: sys::ValueRef,
    ) -> Result<Local<'cx>, JsError> {
        if !exception.is_null() {
            return Err(JsError::Exception(exception_to_owned(self.raw, exception)));
        }
        let value = NonNull::new(value.cast_mut())
            .ok_or(JsError::Backend("JavaScriptCore returned a null value"))?;
        Ok(self.root_local(value))
    }

    fn root_local(&self, value: NonNull<sys::OpaqueValue>) -> Local<'cx> {
        // SAFETY: This value was returned by this live context or resolved from
        // its protected-root registry. Type inspection does not coerce it.
        let kind = unsafe { sys::value_get_type(self.raw.as_ptr(), value.as_ptr()) };
        if !matches!(
            kind,
            sys::TYPE_UNDEFINED | sys::TYPE_NULL | sys::TYPE_BOOLEAN | sys::TYPE_NUMBER
        ) {
            // SAFETY: The context keeps this independent protection until Drop,
            // before the enclosing host entry guard is released.
            unsafe { sys::value_protect(self.raw.as_ptr(), value.as_ptr()) };
            self.local_roots.borrow_mut().push(value);
            #[cfg(test)]
            self.shared
                .context_local_roots
                .set(self.shared.context_local_roots.get() + 1);
        }
        Local::new(Rc::as_ptr(self.shared), value)
    }
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        for value in self.local_roots.get_mut().drain() {
            // SAFETY: Each stored value owns one protection in this context;
            // the host entry still keeps the context alive during unwinding too.
            unsafe { sys::value_unprotect(self.raw.as_ptr(), value.as_ptr()) };
            #[cfg(test)]
            self.shared
                .context_local_roots
                .set(self.shared.context_local_roots.get() - 1);
        }
    }
}

impl Local<'_> {
    fn new(runtime: *const Shared, value: NonNull<sys::OpaqueValue>) -> Self {
        Self {
            runtime,
            value,
            _scope: PhantomData,
            _affine: PhantomData,
        }
    }
}

impl fmt::Debug for Local<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Local(..)")
    }
}

impl Clone for Persistent {
    fn clone(&self) -> Self {
        Self {
            lease: Rc::clone(&self.lease),
        }
    }
}

impl fmt::Debug for Persistent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Persistent(..)")
    }
}

impl Call<'_> {
    /// Returns the number of provided arguments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.arguments.len()
    }

    /// Returns whether no arguments were provided.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arguments.is_empty()
    }

    /// Reads a numeric argument without coercion.
    ///
    /// # Errors
    ///
    /// Returns an error when the argument is missing or is not a number.
    pub fn number(&self, index: usize) -> Result<f64, HostError> {
        strict_number(self.raw_context, self.argument(index)?)
            .map_err(|error| HostError::from_js(&error))
    }

    /// Reads a Boolean argument without coercion.
    ///
    /// # Errors
    ///
    /// Returns an error when the argument is missing or is not a Boolean.
    pub fn boolean(&self, index: usize) -> Result<bool, HostError> {
        strict_boolean(self.raw_context, self.argument(index)?)
            .map_err(|error| HostError::from_js(&error))
    }

    /// Converts an argument to a Rust string.
    ///
    /// # Errors
    ///
    /// Returns an error when the argument is missing or conversion fails.
    pub fn string(&self, index: usize) -> Result<String, HostError> {
        value_to_string(self.raw_context, self.argument(index)?)
            .map_err(|error| HostError::from_js(&error))
    }

    fn argument(&self, index: usize) -> Result<sys::ValueRef, HostError> {
        self.arguments
            .get(index)
            .copied()
            .ok_or_else(|| HostError::new(format!("missing argument at index {index}")))
    }
}

impl HostError {
    /// Creates a host-function error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn from_js(error: &JsError) -> Self {
        Self::new(error.to_string())
    }
}

impl JsException {
    /// Returns the owned exception message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HostError {}

impl fmt::Display for JsException {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for JsException {}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::CreationFailed => "JavaScriptCore runtime creation failed",
            Self::Invalidated => "runtime is invalidated",
            Self::WrongThread => "runtime entered from the wrong thread",
            Self::WrongRuntime => "handle belongs to another runtime",
            Self::StaleHandle => "handle is stale",
            Self::IdentityExhausted => "runtime identity space is exhausted",
            Self::Host(error) => return error.fmt(formatter),
        };
        formatter.write_str(message)
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Host(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::Exception(error) => write!(formatter, "JavaScript exception: {error}"),
            Self::Type { expected } => write!(formatter, "expected JavaScript {expected}"),
            Self::Backend(message) => formatter.write_str(message),
        }
    }
}

impl Error for JsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::Exception(error) => Some(error),
            Self::Type { .. } | Self::Backend(_) => None,
        }
    }
}

impl Shared {
    fn drop_callback(&self, entry: HostFunctionEntry) {
        if contain_unwind(std::panic::AssertUnwindSafe(|| drop(entry))).is_err() {
            self.callback_drop_panics
                .set(self.callback_drop_panics.get().saturating_add(1));
        }
    }

    fn ensure_thread(&self) -> Result<(), RuntimeError> {
        if self.owner == thread::current().id() {
            Ok(())
        } else {
            Err(RuntimeError::WrongThread)
        }
    }

    fn ensure_active(&self) -> Result<(), RuntimeError> {
        self.ensure_thread()?;
        if self.gate.state() == HostState::Active {
            Ok(())
        } else {
            Err(RuntimeError::Invalidated)
        }
    }

    fn drain_native_finalizers(&self) {
        let finalized = self.native_finalizers.take();
        native_state::reclaim_finalized(self, finalized);
    }

    fn close_native_finalizers(&self) {
        let finalized = self.native_finalizers.close();
        native_state::reclaim_finalized(self, finalized);
    }
}

impl Drop for RootLease {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        if runtime.ensure_thread().is_err() || runtime.gate.state() != HostState::Active {
            return;
        }
        let Some(context) = runtime.context.get() else {
            return;
        };
        let Some(value) = runtime.roots.borrow_mut().remove(self.id) else {
            return;
        };

        // SAFETY: This is the last Rust lease, the registry removed the matching
        // generation, and the owning context/thread are still active.
        unsafe { sys::value_unprotect(context.as_ptr(), value.as_ptr()) };
    }
}

impl RootRegistry {
    fn insert(&mut self, value: NonNull<sys::OpaqueValue>) -> RootId {
        if let Some(slot) = self.free.pop() {
            let entry = &mut self.slots[slot];
            entry.value = Some(value);
            return RootId {
                slot,
                generation: entry.generation,
            };
        }

        let slot = self.slots.len();
        self.slots.push(RootSlot {
            generation: 1,
            value: Some(value),
        });
        RootId {
            slot,
            generation: 1,
        }
    }

    fn get(&self, id: RootId) -> Option<NonNull<sys::OpaqueValue>> {
        let slot = self.slots.get(id.slot)?;
        (slot.generation == id.generation)
            .then_some(slot.value)
            .flatten()
    }

    fn remove(&mut self, id: RootId) -> Option<NonNull<sys::OpaqueValue>> {
        let slot = self.slots.get_mut(id.slot)?;
        if slot.generation != id.generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.saturating_add(1);
        if slot.generation != u64::MAX {
            self.free.push(id.slot);
        }
        Some(value)
    }

    fn drain(&mut self) -> Vec<NonNull<sys::OpaqueValue>> {
        let mut values = Vec::new();
        self.free.clear();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(value) = slot.value.take() {
                values.push(value);
            }
            slot.generation = slot.generation.saturating_add(1);
            if slot.generation != u64::MAX {
                self.free.push(index);
            }
        }
        values
    }
}

impl ActiveRuntimeGuard {
    fn enter(runtime: *const Shared) -> Self {
        let previous = ACTIVE_RUNTIME.with(|active| active.replace(runtime));
        Self { previous }
    }
}

impl Drop for ActiveRuntimeGuard {
    fn drop(&mut self) {
        ACTIVE_RUNTIME.with(|active| active.set(self.previous));
    }
}

impl JsString {
    fn new(value: &str) -> Result<Self, JsError> {
        let utf16 = value.encode_utf16().collect::<Vec<_>>();
        // SAFETY: JSC copies `utf16.len()` initialized code units before this vector
        // is dropped. Empty slices provide a valid pointer with zero length.
        let string = unsafe { sys::string_create_with_characters(utf16.as_ptr(), utf16.len()) };
        NonNull::new(string)
            .map(Self)
            .ok_or(JsError::Backend("JavaScriptCore string creation failed"))
    }

    fn as_ptr(&self) -> sys::StringRef {
        self.0.as_ptr()
    }
}

impl Drop for JsString {
    fn drop(&mut self) {
        // SAFETY: `JsString` uniquely owns one reference created by JSC.
        unsafe { sys::string_release(self.0.as_ptr()) };
    }
}

unsafe extern "C" fn host_function_callback(
    context: sys::ContextRef,
    function: sys::ObjectRef,
    _this_object: sys::ObjectRef,
    argument_count: usize,
    arguments: *const sys::ValueRef,
    exception: *mut sys::ValueRef,
) -> sys::ValueRef {
    let result = contain_unwind(std::panic::AssertUnwindSafe(|| {
        let shared = ACTIVE_RUNTIME.with(Cell::get);
        let shared = NonNull::new(shared.cast_mut()).ok_or_else(|| {
            HostError::new("host function called outside an active RustJSI entry")
        })?;

        // SAFETY: `ACTIVE_RUNTIME` is installed from a live `Rc<Shared>` for the
        // duration of runtime entry and restored only after nested execution returns.
        let shared = unsafe { shared.as_ref() };
        if shared.context.get().map(NonNull::as_ptr) != Some(context.cast_mut()) {
            return Err(HostError::new(
                "host function called with the wrong JSC context",
            ));
        }

        let arguments = if argument_count == 0 {
            &[]
        } else {
            if arguments.is_null() {
                return Err(HostError::new("JSC supplied null arguments"));
            }
            // SAFETY: JSC guarantees an array of `argument_count` values for the
            // synchronous callback frame. `Call` cannot escape the callback.
            unsafe { std::slice::from_raw_parts(arguments, argument_count) }
        };
        let key = function as usize;
        let callback = shared
            .host_functions
            .borrow()
            .get(&key)
            .map(|entry| Rc::clone(&entry.callback))
            .ok_or_else(|| HostError::new("host function registration is stale"))?;
        callback(Call {
            raw_context: NonNull::new(context.cast_mut())
                .ok_or_else(|| HostError::new("JSC supplied a null context"))?,
            arguments,
            _affine: PhantomData,
        })
    }));

    match result {
        Ok(Ok(value)) => match value_to_raw_callback(context, &value) {
            Ok(value) => value,
            Err(error) => {
                write_exception(context, exception, error.message());
                // SAFETY: JSC supplied a live callback context.
                unsafe { sys::value_make_undefined(context) }
            }
        },
        Ok(Err(error)) => {
            write_exception(context, exception, error.message());
            // SAFETY: JSC supplied a live callback context.
            unsafe { sys::value_make_undefined(context) }
        }
        Err(()) => {
            write_exception(context, exception, "Rust host function panicked");
            // SAFETY: JSC supplied a live callback context.
            unsafe { sys::value_make_undefined(context) }
        }
    }
}

fn write_exception(context: sys::ContextRef, output: *mut sys::ValueRef, message: &str) {
    if output.is_null() {
        return;
    }
    let Ok(message_string) = JsString::new(message) else {
        return;
    };

    // SAFETY: The message string and callback context are live. The error result and
    // any nested exception are valid for assignment to JSC's exception out-pointer.
    unsafe {
        let message_value = sys::value_make_string(context, message_string.as_ptr());
        let arguments = [message_value];
        let mut nested = ptr::null();
        let error = sys::object_make_error(context, 1, arguments.as_ptr(), &raw mut nested);
        *output = if nested.is_null() { error } else { nested };
    }
}

fn strict_number(
    context: NonNull<sys::OpaqueContext>,
    value: sys::ValueRef,
) -> Result<f64, JsError> {
    // SAFETY: Both handles belong to the current callback or context scope.
    if !unsafe { sys::value_is_number(context.as_ptr(), value) } {
        return Err(JsError::Type { expected: "number" });
    }
    let mut exception = ptr::null();
    // SAFETY: Strict type checking above avoids coercion; exception is still captured.
    let number = unsafe { sys::value_to_number(context.as_ptr(), value, &raw mut exception) };
    if exception.is_null() {
        Ok(number)
    } else {
        Err(JsError::Exception(exception_to_owned(context, exception)))
    }
}

fn strict_boolean(
    context: NonNull<sys::OpaqueContext>,
    value: sys::ValueRef,
) -> Result<bool, JsError> {
    // SAFETY: Both handles belong to the current callback or context scope.
    if !unsafe { sys::value_is_boolean(context.as_ptr(), value) } {
        return Err(JsError::Type {
            expected: "Boolean",
        });
    }
    // SAFETY: Strict type checking above avoids conversion-side execution.
    Ok(unsafe { sys::value_to_boolean(context.as_ptr(), value) })
}

fn value_to_string(
    context: NonNull<sys::OpaqueContext>,
    value: sys::ValueRef,
) -> Result<String, JsError> {
    let mut exception = ptr::null();
    // SAFETY: Both handles are live for this synchronous conversion; any JavaScript
    // exception is captured instead of crossing Rust.
    let string = unsafe { sys::value_to_string_copy(context.as_ptr(), value, &raw mut exception) };
    if !exception.is_null() {
        return Err(JsError::Exception(exception_to_owned(context, exception)));
    }
    let string =
        NonNull::new(string).ok_or(JsError::Backend("JavaScriptCore returned a null string"))?;
    let result = copy_js_string(string);
    // SAFETY: `value_to_string_copy` returned one owned string reference.
    unsafe { sys::string_release(string.as_ptr()) };
    result
}

fn exception_to_owned(
    context: NonNull<sys::OpaqueContext>,
    exception: sys::ValueRef,
) -> JsException {
    let mut nested = ptr::null();
    // SAFETY: The exception is live in this context. A secondary exception is
    // captured and converted to a bounded fallback below.
    let string = unsafe { sys::value_to_string_copy(context.as_ptr(), exception, &raw mut nested) };
    if !nested.is_null() {
        return JsException {
            message: "exception could not be converted to a string".to_owned(),
        };
    }
    let Some(string) = NonNull::new(string) else {
        return JsException {
            message: "JavaScriptCore returned an unprintable exception".to_owned(),
        };
    };
    let message = copy_js_string(string)
        .unwrap_or_else(|_| "JavaScriptCore returned an invalid exception string".to_owned());
    // SAFETY: `value_to_string_copy` returned one owned string reference.
    unsafe { sys::string_release(string.as_ptr()) };
    JsException { message }
}

fn copy_js_string(string: NonNull<sys::OpaqueString>) -> Result<String, JsError> {
    // SAFETY: `string` is live for both calls and the allocated buffer matches the
    // maximum size reported by JSC.
    let maximum = unsafe { sys::string_maximum_utf8_size(string.as_ptr()) };
    if maximum == 0 {
        return Err(JsError::Backend(
            "JavaScriptCore reported an invalid string size",
        ));
    }
    let mut bytes = vec![0_u8; maximum];
    // SAFETY: The buffer contains `maximum` writable bytes as required by JSC.
    let written =
        unsafe { sys::string_get_utf8(string.as_ptr(), bytes.as_mut_ptr().cast(), maximum) };
    if written == 0 || written > maximum {
        return Err(JsError::Backend("JavaScriptCore string conversion failed"));
    }
    bytes.truncate(written - 1);
    String::from_utf8(bytes).map_err(|_| JsError::Backend("JSC produced invalid UTF-8"))
}

fn value_to_raw(
    context: NonNull<sys::OpaqueContext>,
    value: &Value,
    strings: &mut Vec<JsString>,
) -> Result<sys::ValueRef, JsError> {
    match value {
        Value::Undefined => {
            // SAFETY: The context is active for the complete call.
            Ok(unsafe { sys::value_make_undefined(context.as_ptr()) })
        }
        Value::Null => {
            // SAFETY: The context is active for the complete call.
            Ok(unsafe { sys::value_make_null(context.as_ptr()) })
        }
        Value::Boolean(value) => {
            // SAFETY: The context is active for the complete call.
            Ok(unsafe { sys::value_make_boolean(context.as_ptr(), *value) })
        }
        Value::Number(value) => {
            // SAFETY: The context is active for the complete call.
            Ok(unsafe { sys::value_make_number(context.as_ptr(), *value) })
        }
        Value::String(value) => {
            strings.push(JsString::new(value)?);
            let string = strings.last().expect("just pushed a JSC string");
            // SAFETY: The context and stored string remain live through the call.
            Ok(unsafe { sys::value_make_string(context.as_ptr(), string.as_ptr()) })
        }
    }
}

fn value_to_raw_callback(
    context: sys::ContextRef,
    value: &Value,
) -> Result<sys::ValueRef, HostError> {
    let context = NonNull::new(context.cast_mut())
        .ok_or_else(|| HostError::new("JSC supplied a null context"))?;
    let mut strings = Vec::new();
    value_to_raw(context, value, &mut strings).map_err(|error| HostError::from_js(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn retained_roots(shared: &Shared) -> usize {
        shared
            .roots
            .borrow()
            .slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }

    fn object_root(runtime: &mut Runtime) -> Persistent {
        runtime
            .with_context(|cx| {
                let local = cx.eval("({ answer: 42 })", "release.js").unwrap();
                cx.persist(&local).unwrap()
            })
            .unwrap()
    }

    #[test]
    fn last_lease_drop_waits_for_either_host_entry_path() {
        for common_entry in [false, true] {
            let mut runtime = Runtime::new().unwrap();
            let root = object_root(&mut runtime);
            let shared = Rc::clone(&runtime.shared);
            let clone = root.clone();
            drop(root);
            assert_eq!(retained_roots(&shared), 1);
            drop(clone);
            assert_eq!(shared.gate.active_entries(), 0);
            assert_eq!(retained_roots(&shared), 1, "Drop must only request release");
            if common_entry {
                runtime
                    .with_backend(|_| assert_eq!(retained_roots(&shared), 0))
                    .unwrap();
            } else {
                runtime
                    .with_context(|_| assert_eq!(retained_roots(&shared), 0))
                    .unwrap();
            }
        }
    }

    #[test]
    fn lease_drop_inside_entry_waits_for_exit_maintenance() {
        for common_entry in [false, true] {
            let mut runtime = Runtime::new().unwrap();
            let root = object_root(&mut runtime);
            let shared = Rc::clone(&runtime.shared);
            let operation = || {
                drop(root);
                assert_eq!(retained_roots(&shared), 1);
            };
            if common_entry {
                runtime.with_backend(|_| operation()).unwrap();
            } else {
                runtime.with_context(|_| operation()).unwrap();
            }
            assert_eq!(retained_roots(&shared), 0);
        }
    }

    #[test]
    fn shutdown_releases_pending_and_live_roots() {
        let mut runtime = Runtime::new().unwrap();
        let pending = object_root(&mut runtime);
        let live = object_root(&mut runtime);
        let shared = Rc::clone(&runtime.shared);
        drop(pending);
        assert_eq!(retained_roots(&shared), 2);
        runtime.invalidate().unwrap();
        assert_eq!(retained_roots(&shared), 0);
        drop(live);
        runtime.invalidate().unwrap();
        assert_eq!(retained_roots(&shared), 0);
    }

    #[test]
    fn context_locals_in_heap_storage_have_balanced_roots() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let mut values = Vec::new();
                for index in 0..40 {
                    values.push(
                        cx.eval(
                            &format!("({{ toString() {{ return 'object-{index}'; }} }})"),
                            "heap.js",
                        )
                        .unwrap(),
                    );
                }
                assert_eq!(shared.context_local_roots.get(), 40);
                cx.collect_garbage().unwrap();
                for (index, value) in values.iter().enumerate() {
                    assert_eq!(cx.string(value).unwrap(), format!("object-{index}"));
                }
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
    }

    #[test]
    fn resolved_local_survives_last_persistent_lease_drop() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let persistent = runtime
            .with_context(|cx| {
                let value = cx
                    .eval("({ toString() { return 'kept'; } })", "persist.js")
                    .unwrap();
                cx.persist(&value).unwrap()
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
        runtime
            .with_context(|cx| {
                let local = Box::new(cx.resolve(&persistent).unwrap());
                drop(persistent);
                assert_eq!(shared.context_local_roots.get(), 1);
                cx.collect_garbage().unwrap();
                assert_eq!(cx.string(&local).unwrap(), "kept");
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
    }

    #[test]
    fn context_call_roots_strings_but_not_scalar_results() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let text = cx
                    .install_host_function("text", |_| Ok(Value::String("kept string".to_owned())))
                    .unwrap();
                let scalar = cx
                    .install_host_function("scalar", |_| Ok(Value::Number(42.0)))
                    .unwrap();
                let local = Box::new(cx.call(&text, &[]).unwrap());
                assert_eq!(shared.context_local_roots.get(), 1);
                for _ in 0..100 {
                    let value = cx.call(&scalar, &[]).unwrap();
                    assert!((cx.number(&value).unwrap() - 42.0).abs() < f64::EPSILON);
                }
                assert_eq!(shared.context_local_roots.get(), 1);
                cx.collect_garbage().unwrap();
                assert_eq!(cx.string(&local).unwrap(), "kept string");
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
    }

    #[test]
    fn context_root_cleanup_runs_on_unwind() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime
                .with_context(|cx| {
                    let _local = cx.eval("({})", "unwind.js").unwrap();
                    assert_eq!(shared.context_local_roots.get(), 1);
                    panic!("context unwind");
                })
                .unwrap();
        }));
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().downcast_ref::<&str>(),
            Some(&"context unwind")
        );
        assert_eq!(shared.context_local_roots.get(), 0);
        assert_eq!(shared.gate.active_entries(), 0);
        runtime.invalidate().unwrap();
    }

    #[test]
    fn context_roots_managed_kinds_without_retaining_scalar_primitives() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                for source in ["undefined", "null", "true", "NaN", "Infinity", "-0"] {
                    let _value = cx.eval(source, "scalar.js").unwrap();
                }
                assert_eq!(shared.context_local_roots.get(), 0);
                let symbol = Box::new(cx.eval("Symbol('local')", "symbol.js").unwrap());
                let bigint = Box::new(cx.eval("123n", "bigint.js").unwrap());
                let string = Box::new(cx.eval("'rooted string'", "string.js").unwrap());
                assert_eq!(shared.context_local_roots.get(), 3);
                cx.collect_garbage().unwrap();
                assert_eq!(cx.string(&bigint).unwrap(), "123");
                assert_eq!(cx.string(&string).unwrap(), "rooted string");
                let root = cx.persist(&symbol).unwrap();
                drop(root);
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
    }

    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    struct ThreadDropProbe {
        drops: Rc<Cell<usize>>,
        threads: Rc<RefCell<Vec<ThreadId>>>,
    }

    impl Drop for ThreadDropProbe {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
            self.threads.borrow_mut().push(thread::current().id());
        }
    }

    struct NativeResource {
        answer: u32,
        _probe: ThreadDropProbe,
    }

    struct PanicDrop;

    struct CallbackDropProbe {
        shared: Weak<Shared>,
        observations: Rc<RefCell<Vec<bool>>>,
        panic: bool,
    }

    impl Drop for CallbackDropProbe {
        fn drop(&mut self) {
            let shared = self.shared.upgrade().unwrap();
            self.observations
                .borrow_mut()
                .push(shared.host_functions.try_borrow_mut().is_ok());
            assert!(!self.panic, "callback capture drop panic");
        }
    }

    #[test]
    fn callback_teardown_contains_each_drop_without_registry_borrow() {
        let mut runtime = Runtime::new().unwrap();
        let observations = Rc::new(RefCell::new(Vec::new()));
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                for index in 0..4 {
                    let probe = CallbackDropProbe {
                        shared: Rc::downgrade(&shared),
                        observations: Rc::clone(&observations),
                        panic: index == 1,
                    };
                    cx.install_host_function(&format!("callback{index}"), move |_| {
                        let _probe = &probe;
                        Ok(Value::Undefined)
                    })
                    .unwrap();
                }
            })
            .unwrap();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.invalidate()));
        assert!(result.is_ok(), "capture panic escaped invalidation");
        result.unwrap().unwrap();
        assert_eq!(&*observations.borrow(), &[true; 4]);
        assert!(shared.context.get().is_none());
        assert_eq!(shared.gate.state(), HostState::Destroyed);
        assert_eq!(runtime.callback_drop_panics(), 1);
        runtime.invalidate().unwrap();
        assert_eq!(&*observations.borrow(), &[true; 4]);
    }

    #[test]
    fn callback_dispatch_releases_registry_borrow() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let weak = Rc::downgrade(&shared);
                cx.install_host_function("inspectRegistry", move |_| {
                    let shared = weak.upgrade().unwrap();
                    Ok(Value::Boolean(
                        shared.host_functions.try_borrow_mut().is_ok(),
                    ))
                })
                .unwrap();
                let result = cx.eval("inspectRegistry()", "registry.js").unwrap();
                assert!(cx.boolean(&result).unwrap());
            })
            .unwrap();
    }

    #[test]
    fn callback_publication_failure_preserves_exception_and_reclaims_capture() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let observations = Rc::new(RefCell::new(Vec::new()));
        runtime.with_context(|cx| {
            cx.eval("Object.defineProperty(globalThis, 'rejectCallback', { set(value) { globalThis.savedRejected = value; throw new Error('publication failed'); } })", "setter.js").unwrap();
            let probe = CallbackDropProbe {
                shared: Rc::downgrade(&shared),
                observations: Rc::clone(&observations),
                panic: true,
            };
            let error = cx.install_host_function("rejectCallback", move |_| {
                let _probe = &probe;
                Ok(Value::Undefined)
            }).err().expect("publication must fail");
            assert!(matches!(error, JsError::Exception(ref error) if error.message().contains("publication failed")));
            assert!(shared.host_functions.borrow().is_empty());
            let stale = cx.eval("savedRejected()", "stale.js").err().unwrap();
            assert!(stale.to_string().contains("registration is stale"));
            let value = cx.eval("42", "after-failure.js").unwrap();
            assert!((cx.number(&value).unwrap() - 42.0).abs() < f64::EPSILON);
        }).unwrap();
        assert_eq!(&*observations.borrow(), &[true]);
        assert_eq!(runtime.callback_drop_panics(), 1);
        runtime.invalidate().unwrap();
        assert_eq!(&*observations.borrow(), &[true]);
    }

    #[test]
    fn runtime_drop_during_unwind_contains_callback_capture_panic() {
        let observations = Rc::new(RefCell::new(Vec::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut runtime = Runtime::new().unwrap();
            let probe = CallbackDropProbe {
                shared: Rc::downgrade(&runtime.shared),
                observations: Rc::clone(&observations),
                panic: true,
            };
            runtime
                .with_context(|cx| {
                    cx.install_host_function("panicOnDrop", move |_| {
                        let _probe = &probe;
                        Ok(Value::Undefined)
                    })
                    .unwrap();
                })
                .unwrap();
            panic!("outer application unwind");
        }));
        assert!(result.is_err());
        assert_eq!(&*observations.borrow(), &[true]);
    }

    struct PanickingPayload;

    impl Drop for PanickingPayload {
        fn drop(&mut self) {
            panic!("payload drop panic");
        }
    }

    struct DropWithPanickingPayload;

    impl Drop for DropWithPanickingPayload {
        fn drop(&mut self) {
            std::panic::panic_any(PanickingPayload);
        }
    }

    #[test]
    fn native_state_payload_drop_panic_does_not_interrupt_teardown() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|cx| {
                cx.install_native_state("panicState", DropWithPanickingPayload)
                    .unwrap();
            })
            .unwrap();
        runtime.invalidate().unwrap();
        assert_eq!(runtime.shared.native_drop_panics.get(), 1);
        assert_eq!(runtime.shared.gate.state(), HostState::Destroyed);
        assert!(runtime.shared.context.get().is_none());
    }

    #[test]
    fn callback_payload_drop_panic_is_translated_at_c_boundary() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|cx| {
                cx.install_host_function("panicWithPayload", |_| {
                    std::panic::panic_any(PanickingPayload)
                })
                .unwrap();
                let error = cx
                    .eval("panicWithPayload()", "panic-payload.js")
                    .err()
                    .unwrap();
                assert!(error.to_string().contains("Rust host function panicked"));
                let value = cx.eval("42", "after-panic.js").unwrap();
                assert!((cx.number(&value).unwrap() - 42.0).abs() < f64::EPSILON);
            })
            .unwrap();
        runtime.invalidate().unwrap();
    }

    impl Drop for PanicDrop {
        fn drop(&mut self) {
            panic!("native-state destructor panic");
        }
    }

    #[test]
    fn evaluates_values_and_captures_exceptions() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                let value = context.eval("6 * 7", "eval-test.js").unwrap();
                assert!((context.number(&value).unwrap() - 42.0).abs() < f64::EPSILON);

                let error = context
                    .eval("throw new Error('boom')", "throw-test.js")
                    .unwrap_err();
                assert!(matches!(error, JsError::Exception(_)));
                assert!(error.to_string().contains("boom"));
            })
            .unwrap();
    }

    #[test]
    fn calls_stateful_rust_from_javascript_and_rust() {
        let mut runtime = Runtime::new().unwrap();
        let calls = Rc::new(Cell::new(0));
        runtime
            .with_context(|context| {
                let callback_calls = Rc::clone(&calls);
                let add = context
                    .install_host_function("rustAdd", move |call| {
                        callback_calls.set(callback_calls.get() + 1);
                        Ok(Value::Number(call.number(0)? + call.number(1)?))
                    })
                    .unwrap();

                let from_js = context.eval("rustAdd(20, 22)", "callback-test.js").unwrap();
                assert!((context.number(&from_js).unwrap() - 42.0).abs() < f64::EPSILON);

                let from_rust = context
                    .call(&add, &[Value::Number(19.0), Value::Number(23.0)])
                    .unwrap();
                assert!((context.number(&from_rust).unwrap() - 42.0).abs() < f64::EPSILON);
                assert_eq!(calls.get(), 2);
            })
            .unwrap();
    }

    #[test]
    fn translates_host_errors_and_panics_to_javascript_exceptions() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                context
                    .install_host_function("fails", |_| Err(HostError::new("native failure")))
                    .unwrap();
                let failure = context.eval("fails()", "host-error.js").unwrap_err();
                assert!(failure.to_string().contains("native failure"));

                context
                    .install_host_function("panics", |_| panic!("must not unwind into JSC"))
                    .unwrap();
                let panic = context.eval("panics()", "host-panic.js").unwrap_err();
                assert!(panic.to_string().contains("Rust host function panicked"));
            })
            .unwrap();
    }

    #[test]
    fn persistent_handles_are_runtime_scoped_and_invalidated() {
        let mut first = Runtime::new().unwrap();
        let persistent = first
            .with_context(|context| {
                let local = context.eval("({ answer: 42 })", "root-test.js").unwrap();
                context.persist(&local).unwrap()
            })
            .unwrap();

        first
            .with_context(|context| {
                context.collect_garbage().unwrap();
                let local = context.resolve(&persistent).unwrap();
                assert!(context.string(&local).unwrap().contains("object Object"));
            })
            .unwrap();

        let mut second = Runtime::new().unwrap();
        second
            .with_context(|context| {
                assert_eq!(
                    context.resolve(&persistent).unwrap_err(),
                    JsError::Runtime(RuntimeError::WrongRuntime)
                );
            })
            .unwrap();

        first.invalidate().unwrap();
        assert_eq!(
            first.with_context(|_| ()).unwrap_err(),
            RuntimeError::Invalidated
        );
        drop(persistent);
    }

    #[test]
    fn rejects_cross_runtime_locals_before_entering_jsc() {
        let mut first = Runtime::new().unwrap();
        let mut second = Runtime::new().unwrap();
        first
            .with_context(|first_context| {
                let local = first_context.eval("42", "first.js").unwrap();
                second
                    .with_context(|second_context| {
                        assert_eq!(
                            second_context.number(&local).unwrap_err(),
                            JsError::Runtime(RuntimeError::WrongRuntime)
                        );
                        assert_eq!(
                            second_context.persist(&local).unwrap_err(),
                            JsError::Runtime(RuntimeError::WrongRuntime)
                        );
                    })
                    .unwrap();
            })
            .unwrap();
    }

    #[test]
    fn invalidation_is_idempotent() {
        let mut runtime = Runtime::new().unwrap();
        runtime.invalidate().unwrap();
        runtime.invalidate().unwrap();
    }

    #[test]
    fn both_entry_paths_release_admission_after_unwinding() {
        use rustjsi_backend::{BackendBase, BackendScope};

        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.with_context(|_| {
                assert_eq!(shared.gate.active_entries(), 1);
                panic!("context entry panic");
            })
        }));
        assert!(panic.is_err());
        assert_eq!(shared.gate.active_entries(), 0);
        assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.with_backend(|backend| {
                assert_eq!(shared.gate.active_entries(), 1);
                let scope = backend.open_scope().unwrap();
                let _value = scope.evaluate("({answer: 42})", "panic.js").unwrap();
                panic!("common entry panic");
            })
        }));
        assert!(panic.is_err());
        assert_eq!(shared.gate.active_entries(), 0);
        assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());
        runtime
            .with_context(|cx| {
                let value = cx.eval("42", "after.js").unwrap();
                assert!((cx.number(&value).unwrap() - 42.0).abs() < f64::EPSILON);
            })
            .unwrap();
        runtime.invalidate().unwrap();
        assert_eq!(shared.gate.state(), HostState::Destroyed);
    }

    #[test]
    fn busy_invalidation_preserves_engine_and_callback_state_until_retry() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let drops = Rc::new(Cell::new(0));
        let probe = DropProbe(Rc::clone(&drops));
        let persistent = runtime
            .with_context(|cx| {
                cx.install_host_function("ownedCallback", move |_| {
                    let _probe = &probe;
                    Ok(Value::Undefined)
                })
                .unwrap();
                let value = cx.eval("({answer: 42})", "busy.js").unwrap();
                cx.persist(&value).unwrap()
            })
            .unwrap();

        let outer = shared.gate.try_enter().unwrap();
        let inner = shared.gate.try_enter().unwrap();
        let raw = shared.context.get();
        assert_eq!(
            runtime.invalidate(),
            Err(RuntimeError::Host(GateError::EntriesRemain(2)))
        );
        assert_eq!(shared.context.get(), raw);
        assert!(shared.roots.borrow().get(persistent.lease.id).is_some());
        assert_eq!(drops.get(), 0);
        assert_eq!(
            runtime.with_context(|_| ()).unwrap_err(),
            RuntimeError::Invalidated
        );
        assert_eq!(
            runtime.with_backend(|_| ()).unwrap_err(),
            RuntimeError::Invalidated
        );
        drop(inner);
        assert_eq!(
            runtime.invalidate(),
            Err(RuntimeError::Host(GateError::EntriesRemain(1)))
        );
        drop(outer);
        assert!(shared.gate.is_drain_ready());
        assert_eq!(shared.context.get(), raw);
        assert_eq!(drops.get(), 0);
        runtime.invalidate().unwrap();
        runtime.invalidate().unwrap();
        assert!(shared.context.get().is_none());
        assert_eq!(shared.gate.state(), HostState::Destroyed);
        assert!(shared.roots.borrow().get(persistent.lease.id).is_none());
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn both_entry_paths_reject_depth_limit_before_calling_user_code() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let mut entries = (0..64)
            .map(|_| shared.gate.try_enter().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            runtime
                .with_context(|_| panic!("must not enter"))
                .unwrap_err(),
            RuntimeError::Host(GateError::DepthLimit)
        );
        assert_eq!(
            runtime
                .with_backend(|_| panic!("must not enter"))
                .unwrap_err(),
            RuntimeError::Host(GateError::DepthLimit)
        );
        assert_eq!(shared.gate.active_entries(), 64);
        drop(entries.pop());
        runtime
            .with_context(|_| assert_eq!(shared.gate.active_entries(), 64))
            .unwrap();
        runtime
            .with_backend(|_| assert_eq!(shared.gate.active_entries(), 64))
            .unwrap();
        drop(entries);
        assert_eq!(shared.gate.active_entries(), 0);
    }

    #[test]
    fn nested_runtime_panic_restores_outer_entry_and_callback_dispatch() {
        let mut first = Runtime::new().unwrap();
        let mut second = Runtime::new().unwrap();
        let first_shared = Rc::clone(&first.shared);
        let second_shared = Rc::clone(&second.shared);
        first
            .with_context(|cx| {
                cx.install_host_function("outerAnswer", |_| Ok(Value::Number(42.0)))
                    .unwrap();
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    second.with_backend(|_| {
                        assert_eq!(first_shared.gate.active_entries(), 1);
                        assert_eq!(second_shared.gate.active_entries(), 1);
                        assert_eq!(ACTIVE_RUNTIME.with(Cell::get), Rc::as_ptr(&second_shared));
                        panic!("nested entry panic");
                    })
                }));
                assert!(panic.is_err());
                assert_eq!(second_shared.gate.active_entries(), 0);
                assert_eq!(ACTIVE_RUNTIME.with(Cell::get), Rc::as_ptr(&first_shared));
                let answer = cx.eval("outerAnswer()", "outer.js").unwrap();
                assert!((cx.number(&answer).unwrap() - 42.0).abs() < f64::EPSILON);
            })
            .unwrap();
        assert_eq!(first_shared.gate.active_entries(), 0);
        assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());
    }

    #[test]
    fn releases_host_callback_state_during_invalidation() {
        let drops = Rc::new(Cell::new(0));
        let probe = DropProbe(Rc::clone(&drops));
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                context
                    .install_host_function("probe", move |_| {
                        let _ = &probe;
                        Ok(Value::Undefined)
                    })
                    .unwrap();
            })
            .unwrap();
        assert_eq!(drops.get(), 0);
        runtime.invalidate().unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn survives_repeated_runtime_lifecycle_cycles() {
        for _ in 0..1_000 {
            let mut runtime = Runtime::new().unwrap();
            runtime
                .with_context(|context| {
                    let local = context.eval("40 + 2", "cycle.js").unwrap();
                    let root = context.persist(&local).unwrap();
                    context.collect_garbage().unwrap();
                    let rooted = context.resolve(&root).unwrap();
                    assert!((context.number(&rooted).unwrap() - 42.0).abs() < f64::EPSILON);

                    let add = context
                        .install_host_function("add", |call| {
                            Ok(Value::Number(call.number(0)? + call.number(1)?))
                        })
                        .unwrap();
                    let result = context
                        .call(&add, &[Value::Number(20.0), Value::Number(22.0)])
                        .unwrap();
                    assert!((context.number(&result).unwrap() - 42.0).abs() < f64::EPSILON);
                })
                .unwrap();
            runtime.invalidate().unwrap();
        }
    }

    #[test]
    fn native_state_is_collected_and_dropped_on_the_runtime_thread() {
        let owner = thread::current().id();
        let drops = Rc::new(Cell::new(0));
        let threads = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new().unwrap();
        let handle = runtime
            .with_context(|context| {
                context
                    .install_native_state(
                        "nativeResource",
                        NativeResource {
                            answer: 42,
                            _probe: ThreadDropProbe {
                                drops: Rc::clone(&drops),
                                threads: Rc::clone(&threads),
                            },
                        },
                    )
                    .unwrap()
            })
            .unwrap();

        runtime
            .with_context(|context| {
                let answer = context
                    .with_native_state(&handle, |state| state.answer)
                    .unwrap();
                assert_eq!(answer, 42);
            })
            .unwrap();

        let mut other = Runtime::new().unwrap();
        other
            .with_context(|context| {
                assert_eq!(
                    context.with_native_state(&handle, |_| ()).unwrap_err(),
                    JsError::Runtime(RuntimeError::WrongRuntime)
                );
            })
            .unwrap();

        runtime
            .with_context(|context| {
                context
                    .eval("delete nativeResource", "native-delete.js")
                    .unwrap();
            })
            .unwrap();
        collect_until(&mut runtime, &drops, 1);
        runtime
            .with_context(|context| {
                assert_eq!(
                    context.with_native_state(&handle, |_| ()).unwrap_err(),
                    JsError::Runtime(RuntimeError::StaleHandle)
                );
            })
            .unwrap();

        assert_eq!(drops.get(), 1);
        assert_eq!(threads.borrow().as_slice(), &[owner]);
    }

    #[test]
    fn finalizer_queue_reclaims_bursts_without_user_drop_in_finalizer() {
        const OBJECTS: usize = 512;

        let owner = thread::current().id();
        let drops = Rc::new(Cell::new(0));
        let threads = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                for index in 0..OBJECTS {
                    context
                        .install_native_state(
                            &format!("native{index}"),
                            ThreadDropProbe {
                                drops: Rc::clone(&drops),
                                threads: Rc::clone(&threads),
                            },
                        )
                        .unwrap();
                }
                context
                    .eval(
                        "for (let i = 0; i < 512; i++) delete this['native' + i]",
                        "native-burst-delete.js",
                    )
                    .unwrap();
            })
            .unwrap();
        collect_until(&mut runtime, &drops, OBJECTS);

        assert_eq!(drops.get(), OBJECTS);
        assert!(threads.borrow().iter().all(|thread| *thread == owner));
    }

    #[test]
    fn invalidation_releases_live_native_state_on_the_runtime_thread() {
        let owner = thread::current().id();
        let drops = Rc::new(Cell::new(0));
        let threads = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                context
                    .install_native_state(
                        "liveResource",
                        ThreadDropProbe {
                            drops: Rc::clone(&drops),
                            threads: Rc::clone(&threads),
                        },
                    )
                    .unwrap();
            })
            .unwrap();

        runtime.invalidate().unwrap();
        assert_eq!(drops.get(), 1);
        assert_eq!(threads.borrow().as_slice(), &[owner]);
    }

    #[test]
    fn contains_native_state_destructor_panics_outside_jsc() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                context
                    .install_native_state("panicState", PanicDrop)
                    .unwrap();
                context
                    .eval("delete panicState", "native-panic-delete.js")
                    .unwrap();
                context.collect_garbage().unwrap();
            })
            .unwrap();

        for _ in 0..32 {
            if runtime.shared.native_drop_panics.get() == 1 {
                break;
            }
            runtime
                .with_context(|context| {
                    context.collect_garbage().unwrap();
                    context
                        .eval(
                            "Array.from({ length: 4096 }, (_, i) => ({ i }))",
                            "native-panic-gc-churn.js",
                        )
                        .unwrap();
                })
                .unwrap();
        }
        assert_eq!(runtime.shared.native_drop_panics.get(), 1);
        runtime.invalidate().unwrap();
    }

    #[test]
    fn external_buffer_preserves_payload_allocation_and_js_mutation() {
        const BYTE_LEN: usize = 1024 * 1024;

        let bytes = (0_u8..=u8::MAX)
            .cycle()
            .take(BYTE_LEN)
            .map(|byte| byte.wrapping_mul(31).wrapping_add(7))
            .collect::<Vec<_>>();
        let checksum = bytes.iter().fold(0.0, |sum, byte| sum + f64::from(*byte));
        let mut runtime = Runtime::new().unwrap();
        let buffer = runtime
            .with_context(|context| {
                context
                    .install_external_buffer("externalPayload", bytes.into_boxed_slice())
                    .unwrap()
            })
            .unwrap();

        assert_eq!(buffer.byte_len(), BYTE_LEN);
        assert_eq!(buffer.backing_store_matches_origin(), Some(true));

        runtime
            .with_context(|context| {
                let observed = context
                    .eval(
                        "(() => { const view = new Uint8Array(externalPayload); let sum = 0;\
                         for (let i = 0; i < view.length; i++) sum += view[i]; return sum; })()",
                        "external-checksum.js",
                    )
                    .unwrap();
                assert!((context.number(&observed).unwrap() - checksum).abs() < f64::EPSILON);

                let mutation = context
                    .eval(
                        "(() => { const view = new Uint8Array(externalPayload); view[0] = 99;\
                         view[view.length - 1] = 17; return view[0] * 256 + view[view.length - 1]; })()",
                        "external-mutation.js",
                    )
                    .unwrap();
                let mutation = context.number(&mutation).unwrap();
                assert!((mutation - f64::from(99 * 256 + 17)).abs() < f64::EPSILON);
                context
                    .eval("delete externalPayload", "external-delete.js")
                    .unwrap();
            })
            .unwrap();

        collect_external_until(&mut runtime, &buffer);
        assert_eq!(buffer.deallocator_received_origin(), Some(true));
        assert_eq!(runtime.shared.external_buffers.live_allocations(), 0);
        assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
        assert_eq!(runtime.shared.external_buffers.deallocations(), 1);
    }

    #[test]
    fn external_buffer_supports_empty_payload() {
        let mut runtime = Runtime::new().unwrap();
        let buffer = runtime
            .with_context(|context| {
                context
                    .install_external_buffer("emptyPayload", Vec::new().into_boxed_slice())
                    .unwrap()
            })
            .unwrap();

        assert_eq!(buffer.byte_len(), 0);
        assert_eq!(buffer.backing_store_matches_origin(), None);
        runtime
            .with_context(|context| {
                let length = context
                    .eval("emptyPayload.byteLength", "empty-buffer.js")
                    .unwrap();
                assert!(context.number(&length).unwrap().abs() < f64::EPSILON);
                context
                    .eval("delete emptyPayload", "empty-delete.js")
                    .unwrap();
            })
            .unwrap();
        collect_external_until(&mut runtime, &buffer);
        assert_eq!(buffer.deallocator_received_origin(), Some(true));
    }

    #[test]
    fn external_buffer_deallocator_survives_runtime_invalidation() {
        let mut runtime = Runtime::new().unwrap();
        let buffer = runtime
            .with_context(|context| {
                context
                    .install_external_buffer("liveExternal", vec![42; 64 * 1024].into_boxed_slice())
                    .unwrap()
            })
            .unwrap();

        runtime.invalidate().unwrap();
        assert!(buffer.is_deallocated());
        assert_eq!(buffer.deallocator_received_origin(), Some(true));
        assert_eq!(runtime.shared.external_buffers.live_allocations(), 0);
        assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
        assert_eq!(runtime.shared.external_buffers.deallocations(), 1);
    }

    #[test]
    fn external_buffer_publication_failure_releases_transferred_owner() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|context| {
                context
                    .eval(
                        "Object.defineProperty(this, 'rejectedExternal', {\
                         set() { throw new Error('publication rejected'); }, configurable: true })",
                        "reject-publication.js",
                    )
                    .unwrap();
                assert!(matches!(
                    context.install_external_buffer(
                        "rejectedExternal",
                        vec![1, 2, 3, 4].into_boxed_slice(),
                    ),
                    Err(JsError::Exception(_))
                ));
            })
            .unwrap();

        collect_external_ledger_until_empty(&mut runtime);
        assert_eq!(runtime.shared.external_buffers.live_allocations(), 0);
        assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
        assert_eq!(runtime.shared.external_buffers.deallocations(), 1);
    }

    #[test]
    fn external_buffer_burst_reconciles_ledger() {
        const BUFFERS: usize = 512;

        let mut runtime = Runtime::new().unwrap();
        let buffers = runtime
            .with_context(|context| {
                let mut buffers = Vec::with_capacity(BUFFERS);
                for index in 0..BUFFERS {
                    buffers.push(
                        context
                            .install_external_buffer(
                                &format!("external{index}"),
                                vec![u8::try_from(index % 256).unwrap(); 257].into_boxed_slice(),
                            )
                            .unwrap(),
                    );
                }
                context
                    .eval(
                        "for (let i = 0; i < 512; i++) delete this['external' + i]",
                        "external-burst-delete.js",
                    )
                    .unwrap();
                buffers
            })
            .unwrap();

        collect_external_ledger_until_empty(&mut runtime);
        assert!(buffers.iter().all(ExternalBuffer::is_deallocated));
        assert!(
            buffers
                .iter()
                .all(|buffer| buffer.deallocator_received_origin() == Some(true))
        );
        assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
        assert_eq!(runtime.shared.external_buffers.deallocations(), BUFFERS);
    }

    fn collect_until(runtime: &mut Runtime, drops: &Cell<usize>, expected: usize) {
        for _ in 0..32 {
            runtime
                .with_context(|context| {
                    context.collect_garbage().unwrap();
                    context
                        .eval(
                            "Array.from({ length: 4096 }, (_, i) => ({ i }))",
                            "native-gc-churn.js",
                        )
                        .unwrap();
                })
                .unwrap();
            if drops.get() == expected {
                return;
            }
        }
        panic!(
            "expected {expected} native-state drops, observed {}",
            drops.get()
        );
    }

    fn collect_external_until(runtime: &mut Runtime, buffer: &ExternalBuffer) {
        for _ in 0..32 {
            if buffer.is_deallocated() {
                return;
            }
            force_external_collection(runtime);
        }
        panic!("external ArrayBuffer owner was not deallocated");
    }

    fn collect_external_ledger_until_empty(runtime: &mut Runtime) {
        for _ in 0..32 {
            if runtime.shared.external_buffers.live_allocations() == 0 {
                return;
            }
            force_external_collection(runtime);
        }
        panic!(
            "expected an empty external ledger, observed {} allocations and {} bytes",
            runtime.shared.external_buffers.live_allocations(),
            runtime.shared.external_buffers.live_bytes()
        );
    }

    fn force_external_collection(runtime: &mut Runtime) {
        runtime
            .with_context(|context| {
                context.collect_garbage().unwrap();
                context
                    .eval(
                        "Array.from({ length: 4096 }, (_, i) => ({ i }))",
                        "external-gc-churn.js",
                    )
                    .unwrap();
            })
            .unwrap();
    }
}
