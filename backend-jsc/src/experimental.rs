// SPDX-License-Identifier: MIT OR Apache-2.0

//! Experimental direct integration with the macOS `JavaScriptCore` C API.

use crate::sys;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::rc::{Rc, Weak};
use std::thread::{self, ThreadId};

thread_local! {
    static ACTIVE_RUNTIME: Cell<*const Shared> = const { Cell::new(ptr::null()) };
}

const INLINE_ARGUMENTS: usize = 8;

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
    _affine: PhantomData<Rc<()>>,
}

/// A JavaScript value valid only during its context entry.
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
    owner: ThreadId,
    lifecycle: Cell<Lifecycle>,
    context: Cell<Option<NonNull<sys::OpaqueContext>>>,
    roots: RefCell<RootRegistry>,
    host_functions: RefCell<HashMap<usize, HostFunctionEntry>>,
}

struct HostFunctionEntry {
    function: NonNull<sys::OpaqueValue>,
    callback: Box<Callback>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Active,
    Draining,
    Invalid,
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
        // SAFETY: A null class requests JSC's default global object class. The returned
        // context is checked before ownership is placed in `Runtime`.
        let context = unsafe { sys::global_context_create(ptr::null_mut()) };
        let context = NonNull::new(context).ok_or(RuntimeError::CreationFailed)?;

        Ok(Self {
            shared: Rc::new(Shared {
                owner: thread::current().id(),
                lifecycle: Cell::new(Lifecycle::Active),
                context: Cell::new(Some(context)),
                roots: RefCell::new(RootRegistry::default()),
                host_functions: RefCell::new(HashMap::new()),
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
        let context = self.shared.context.get().ok_or(RuntimeError::Invalidated)?;
        let _active = ActiveRuntimeGuard::enter(Rc::as_ptr(&self.shared));
        let mut scoped = Context {
            shared: &self.shared,
            raw: context,
            _affine: PhantomData,
        };
        Ok(operation(&mut scoped))
    }

    /// Invalidates handles, releases roots, and destroys the JSC context.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::WrongThread`] when called off the owning thread.
    pub fn invalidate(&mut self) -> Result<(), RuntimeError> {
        self.shared.ensure_thread()?;
        if self.shared.lifecycle.get() == Lifecycle::Invalid {
            return Ok(());
        }

        self.shared.lifecycle.set(Lifecycle::Draining);
        let context = self.shared.context.get().ok_or(RuntimeError::Invalidated)?;
        let roots = self.shared.roots.borrow_mut().drain();
        let functions = self
            .shared
            .host_functions
            .borrow()
            .values()
            .map(|entry| entry.function)
            .collect::<Vec<_>>();

        for value in roots.into_iter().chain(functions) {
            // SAFETY: Every value was protected exactly once in this context and is
            // unprotected before the context is released, on its owning thread.
            unsafe { sys::value_unprotect(context.as_ptr(), value.as_ptr()) };
        }
        self.shared.host_functions.borrow_mut().clear();

        // SAFETY: `Runtime` owns the retained global context, all RustJSI roots have
        // been released, and the owning thread is performing the single release.
        unsafe { sys::global_context_release(context.as_ptr()) };
        self.shared.context.set(None);
        self.shared.lifecycle.set(Lifecycle::Invalid);
        Ok(())
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
        Ok(Local::new(Rc::as_ptr(self.shared), value))
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
                callback: Box::new(callback),
            },
        );

        // SAFETY: The registry balances this protection at invalidation, preventing
        // function-address reuse while the callback map contains the address.
        unsafe { sys::value_protect(self.raw.as_ptr(), function.as_ptr()) };

        // SAFETY: The active context always has a global object.
        let global = unsafe { sys::context_get_global_object(self.raw.as_ptr()) };
        let global = NonNull::new(global).ok_or(JsError::Backend(
            "JavaScriptCore returned a null global object",
        ))?;
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
            self.shared.host_functions.borrow_mut().remove(&key);
            // SAFETY: This balances the protection established above after publication
            // failed, while the context is still active.
            unsafe { sys::value_unprotect(self.raw.as_ptr(), function.as_ptr()) };
            return Err(JsError::Exception(exception_to_owned(self.raw, exception)));
        }

        Ok(HostFunction {
            runtime: Rc::downgrade(self.shared),
            key,
        })
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
        Ok(Local::new(Rc::as_ptr(self.shared), value))
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
        };
        formatter.write_str(message)
    }
}

impl Error for RuntimeError {}

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
    fn ensure_thread(&self) -> Result<(), RuntimeError> {
        if self.owner == thread::current().id() {
            Ok(())
        } else {
            Err(RuntimeError::WrongThread)
        }
    }

    fn ensure_active(&self) -> Result<(), RuntimeError> {
        self.ensure_thread()?;
        if self.lifecycle.get() == Lifecycle::Active {
            Ok(())
        } else {
            Err(RuntimeError::Invalidated)
        }
    }
}

impl Drop for RootLease {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        if runtime.ensure_thread().is_err() || runtime.lifecycle.get() != Lifecycle::Active {
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let callbacks = shared.host_functions.borrow();
        let callback = callbacks
            .get(&key)
            .ok_or_else(|| HostError::new("host function registration is stale"))?;
        (callback.callback)(Call {
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
        Err(_) => {
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

    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
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
}
