// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Context, JsError, JsString, RuntimeError, Shared};
use crate::sys;
use std::any::{Any, TypeId};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

const MAX_NATIVE_STATES: usize = 4_096;

/// A typed handle to Rust state owned by a JavaScript wrapper.
///
/// ```compile_fail
/// use rustjsi_backend_jsc::NativeObject;
/// fn require_send<T: Send>() {}
/// require_send::<NativeObject<usize>>();
/// ```
pub struct NativeObject<T> {
    runtime: Weak<Shared>,
    id: NativeId,
    _type: PhantomData<fn() -> T>,
    _affine: PhantomData<Rc<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeId {
    slot: usize,
    generation: u64,
}

#[derive(Default)]
pub(super) struct NativeRegistry {
    slots: Vec<NativeSlot>,
    free: Vec<usize>,
    live: usize,
}

struct NativeSlot {
    generation: u64,
    type_id: Option<TypeId>,
    value: Option<Rc<dyn Any>>,
}

struct NativeLease<'a, T: 'static> {
    shared: &'a Shared,
    value: Option<Rc<T>>,
}

struct PublicationRoot {
    context: NonNull<sys::OpaqueContext>,
    object: NonNull<sys::OpaqueValue>,
}

impl PublicationRoot {
    fn new(context: NonNull<sys::OpaqueContext>, object: NonNull<sys::OpaqueValue>) -> Self {
        // SAFETY: The caller just created this object inside its active Context.
        // Keep it rooted through setters, exception conversion and rollback.
        unsafe { sys::value_protect(context.as_ptr(), object.as_ptr()) };
        Self { context, object }
    }
}

impl Drop for PublicationRoot {
    fn drop(&mut self) {
        // SAFETY: This stack-local guard cannot outlive the installation entry;
        // its protection is released once, before the Context or host entry ends.
        unsafe { sys::value_unprotect(self.context.as_ptr(), self.object.as_ptr()) };
    }
}

impl<T: 'static> Drop for NativeLease<'_, T> {
    fn drop(&mut self) {
        // Retirement can leave this operation as the last owner, including on
        // unwind. Contain the destructor separately from the user's operation.
        drop_state(
            self.shared,
            self.value.take().map(|value| value as Rc<dyn Any>),
        );
    }
}

pub(super) struct FinalizerQueue {
    head: AtomicPtr<FinalizerToken>,
}

pub(super) struct FinalizerToken {
    queue: Arc<FinalizerQueue>,
    id: NativeId,
    next: AtomicPtr<FinalizerToken>,
}

impl<T> fmt::Debug for NativeObject<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeObject(..)")
    }
}

impl FinalizerQueue {
    pub(super) fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub(super) fn take(&self) -> *mut FinalizerToken {
        loop {
            let head = self.head.load(Ordering::Acquire);
            if head == closed_sentinel() {
                return ptr::null_mut();
            }
            if self
                .head
                .compare_exchange_weak(head, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return head;
            }
        }
    }

    pub(super) fn close(&self) -> *mut FinalizerToken {
        let head = self.head.swap(closed_sentinel(), Ordering::AcqRel);
        if head == closed_sentinel() {
            ptr::null_mut()
        } else {
            head
        }
    }

    unsafe fn push(&self, token: *mut FinalizerToken) {
        loop {
            let head = self.head.load(Ordering::Acquire);
            if head == closed_sentinel() {
                // SAFETY: The finalizer transferred unique token ownership to this
                // method. A closed queue has no consumer, and the token contains no
                // user state or engine handle.
                drop(unsafe { Box::from_raw(token) });
                return;
            }

            // SAFETY: `token` is uniquely owned until a successful publication.
            unsafe { (*token).next.store(head, Ordering::Relaxed) };
            if self
                .head
                .compare_exchange_weak(head, token, Ordering::Release, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

impl NativeRegistry {
    fn insert<T: 'static>(&mut self, value: T) -> Result<NativeId, T> {
        if self.live == MAX_NATIVE_STATES {
            return Err(value);
        }
        self.live += 1;
        if let Some(slot) = self.free.pop() {
            let entry = &mut self.slots[slot];
            entry.type_id = Some(TypeId::of::<T>());
            entry.value = Some(Rc::new(value));
            return Ok(NativeId {
                slot,
                generation: entry.generation,
            });
        }

        let slot = self.slots.len();
        self.slots.push(NativeSlot {
            generation: 1,
            type_id: Some(TypeId::of::<T>()),
            value: Some(Rc::new(value)),
        });
        Ok(NativeId {
            slot,
            generation: 1,
        })
    }

    fn get<T: 'static>(&self, id: NativeId) -> Option<Rc<T>> {
        let slot = self.slots.get(id.slot)?;
        if slot.generation != id.generation || slot.type_id != Some(TypeId::of::<T>()) {
            return None;
        }
        Rc::clone(slot.value.as_ref()?).downcast().ok()
    }

    fn remove(&mut self, id: NativeId) -> Option<Rc<dyn Any>> {
        let slot = self.slots.get_mut(id.slot)?;
        if slot.generation != id.generation {
            return None;
        }
        let value = slot.value.take()?;
        self.live -= 1;
        slot.type_id = None;
        slot.generation = slot.generation.saturating_add(1);
        if slot.generation != u64::MAX {
            self.free.push(id.slot);
        }
        Some(value)
    }

    pub(super) fn drain(&mut self) -> Vec<Rc<dyn Any>> {
        let mut values = Vec::new();
        self.free.clear();
        self.live = 0;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(value) = slot.value.take() {
                values.push(value);
            }
            slot.type_id = None;
            slot.generation = slot.generation.saturating_add(1);
            if slot.generation != u64::MAX {
                self.free.push(index);
            }
        }
        values
    }
}

impl Context<'_> {
    /// Installs Rust state behind an ordinary global JavaScript wrapper object.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, allocation, publication, or JavaScript exception error.
    pub fn install_native_state<T: 'static>(
        &mut self,
        name: &str,
        state: T,
    ) -> Result<NativeObject<T>, JsError> {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        let inserted = {
            let mut states = self.shared.native_states.borrow_mut();
            states.insert(state)
        };
        let id = match inserted {
            Ok(id) => id,
            Err(state) => {
                drop_state(self.shared, Some(Rc::new(state)));
                return Err(JsError::Backend("native-state capacity exceeded"));
            }
        };
        let token = Box::new(FinalizerToken {
            queue: Arc::clone(&self.shared.native_finalizers),
            id,
            next: AtomicPtr::new(ptr::null_mut()),
        });
        let token = Box::into_raw(token);

        // SAFETY: The SDK exports a complete version-zero definition by value. This
        // code changes only the finalize callback before passing a live definition.
        let mut definition = unsafe { sys::CLASS_DEFINITION_EMPTY };
        definition.finalize = Some(native_state_finalize);
        // SAFETY: `definition` matches JSC's C layout and remains live for creation.
        let class = unsafe { sys::class_create(&raw const definition) };
        let Some(class) = NonNull::new(class) else {
            // SAFETY: Ownership was not transferred because class creation failed.
            drop(unsafe { Box::from_raw(token) });
            let state = self.shared.native_states.borrow_mut().remove(id);
            drop_state(self.shared, state);
            return Err(JsError::Backend("JavaScriptCore class creation failed"));
        };

        // SAFETY: The class and context are live. On success JSC owns the private token
        // until its finalize callback; the class retains its definition internally.
        let object = unsafe { sys::object_make(self.raw.as_ptr(), class.as_ptr(), token.cast()) };
        // SAFETY: This balances `class_create`; the object retains the class behavior.
        unsafe { sys::class_release(class.as_ptr()) };
        let Some(object) = NonNull::new(object) else {
            // SAFETY: A null object means JSC did not take ownership of private data.
            drop(unsafe { Box::from_raw(token) });
            let state = self.shared.native_states.borrow_mut().remove(id);
            drop_state(self.shared, state);
            return Err(JsError::Backend("JavaScriptCore object creation failed"));
        };
        let _publication_root = PublicationRoot::new(self.raw, object);

        let property = match JsString::new(name) {
            Ok(property) => property,
            Err(error) => {
                self.rollback_native_object(object, token, id);
                return Err(error);
            }
        };
        // SAFETY: The active context always has a global object.
        let global = unsafe { sys::context_get_global_object(self.raw.as_ptr()) };
        let Some(global) = NonNull::new(global) else {
            self.rollback_native_object(object, token, id);
            return Err(JsError::Backend(
                "JavaScriptCore returned a null global object",
            ));
        };
        let mut exception = ptr::null();
        // SAFETY: The object and property belong to this active context. The exception
        // output is initialized and checked before returning.
        unsafe {
            sys::object_set_property(
                self.raw.as_ptr(),
                global.as_ptr(),
                property.as_ptr(),
                object.as_ptr(),
                0,
                &raw mut exception,
            );
        }
        if !exception.is_null() {
            let exception = super::exception_to_owned(self.raw, exception);
            self.rollback_native_object(object, token, id);
            return Err(JsError::Exception(exception));
        }

        Ok(NativeObject {
            runtime: Rc::downgrade(self.shared),
            id,
            _type: PhantomData,
            _affine: PhantomData,
        })
    }

    /// Leases live typed Rust state for one synchronous operation.
    ///
    /// The registry borrow ends before `operation` starts. Retirement rejects
    /// future accesses but cannot destroy state used by an admitted operation.
    /// Only shared access is exposed; interior-mutability rules belong to `T`.
    ///
    /// ```compile_fail
    /// use rustjsi_backend_jsc::Runtime;
    /// let mut runtime = Runtime::new().unwrap();
    /// runtime.with_context(|cx| {
    ///     let object = cx.install_native_state("object", String::from("state")).unwrap();
    ///     let borrowed = cx.with_native_state(&object, |state| state).unwrap();
    ///     println!("{borrowed}");
    /// }).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error for an inactive runtime or dead, foreign, collected, or
    /// type-invalid handle.
    ///
    /// # Panics
    ///
    /// Propagates an `operation` panic after releasing its lease. An unwinding
    /// state-destructor panic is contained and counted separately.
    pub fn with_native_state<T: 'static, R>(
        &mut self,
        handle: &NativeObject<T>,
        operation: impl FnOnce(&T) -> R,
    ) -> Result<R, JsError> {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        let runtime = handle
            .runtime
            .upgrade()
            .ok_or(JsError::Runtime(RuntimeError::Invalidated))?;
        if !Rc::ptr_eq(&runtime, self.shared) {
            return Err(JsError::Runtime(RuntimeError::WrongRuntime));
        }

        let state = self
            .shared
            .native_states
            .borrow()
            .get(handle.id)
            .ok_or(JsError::Runtime(RuntimeError::StaleHandle))?;
        let lease = NativeLease {
            shared: self.shared,
            value: Some(state),
        };
        Ok(operation(
            lease.value.as_deref().expect("live operation lease"),
        ))
    }

    fn rollback_native_object(
        &self,
        object: NonNull<sys::OpaqueValue>,
        token: *mut FinalizerToken,
        id: NativeId,
    ) {
        // SAFETY: The object was created with a class that stores private data. A
        // successful detach transfers the still-unpublished token back to Rust.
        if unsafe { sys::object_set_private(object.as_ptr(), ptr::null_mut()) } {
            // SAFETY: JSC no longer owns or can finalize this detached token.
            drop(unsafe { Box::from_raw(token) });
            let state = self.shared.native_states.borrow_mut().remove(id);
            drop_state(self.shared, state);
        }
    }
}

pub(super) fn reclaim_finalized(shared: &Shared, mut token: *mut FinalizerToken) -> usize {
    let mut retired = 0;
    while let Some(current) = NonNull::new(token) {
        // SAFETY: The queue transferred unique ownership of this detached list to the
        // runtime thread. `next` was initialized before publication.
        let boxed = unsafe { Box::from_raw(current.as_ptr()) };
        token = boxed.next.load(Ordering::Relaxed);
        let state = shared.native_states.borrow_mut().remove(boxed.id);
        drop(boxed);
        retired += usize::from(state.is_some());
        drop_state(shared, state);
    }
    retired
}

pub(super) fn drop_states(shared: &Shared, states: Vec<Rc<dyn Any>>) {
    for state in states {
        drop_state(shared, Some(state));
    }
}

fn drop_state(shared: &Shared, state: Option<Rc<dyn Any>>) {
    if super::contain_unwind(std::panic::AssertUnwindSafe(|| drop(state))).is_err() {
        shared
            .native_drop_panics
            .set(shared.native_drop_panics.get().saturating_add(1));
    }
}

unsafe extern "C" fn native_state_finalize(object: sys::ObjectRef) {
    // SAFETY: JSC supplies the object being finalized. `JSObjectGetPrivate` has no
    // context parameter and is permitted by JSC's finalizer contract.
    let token = unsafe { sys::object_get_private(object) }.cast::<FinalizerToken>();
    let Some(token) = NonNull::new(token) else {
        return;
    };
    // SAFETY: The private pointer uniquely names the token transferred at object
    // creation. Cloning the queue keeps it alive if invalidation already completed.
    let queue = unsafe { Arc::clone(&token.as_ref().queue) };
    // SAFETY: JSC invokes finalize once for this object, transferring its private token
    // to the lock-free queue. This operation performs no allocation, lock, JSC entry,
    // or user-state destruction.
    unsafe { queue.push(token.as_ptr()) };
}

fn closed_sentinel() -> *mut FinalizerToken {
    ptr::without_provenance_mut(1)
}

#[cfg(test)]
mod tests {
    use super::super::Runtime;
    use super::*;
    use std::cell::Cell;
    use std::mem::{align_of, size_of};

    struct DropProbe {
        value: usize,
        drops: Rc<Cell<usize>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn native_access_releases_the_registry_borrow_before_user_code() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let handle = cx.install_native_state("resource", 42_u32).unwrap();
                cx.with_native_state(&handle, |state| {
                    assert_eq!(*state, 42);
                    assert!(shared.native_states.try_borrow_mut().is_ok());
                })
                .unwrap();
            })
            .unwrap();
    }

    #[test]
    fn active_operation_keeps_retired_state_alive_after_slot_reuse() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let drops = Rc::new(Cell::new(0));
        runtime
            .with_context(|cx| {
                let handle = cx
                    .install_native_state(
                        "resource",
                        DropProbe {
                            value: 41,
                            drops: Rc::clone(&drops),
                        },
                    )
                    .unwrap();
                cx.with_native_state(&handle, |state| {
                    // Simulate retirement by a reentrant host callback, without
                    // introducing a public same-runtime reentry API in this test.
                    let removed = shared.native_states.borrow_mut().remove(handle.id);
                    drop_state(&shared, removed);
                    assert_eq!(drops.get(), 0);
                    let replacement = shared.native_states.borrow_mut().insert(99_u32).unwrap();
                    assert_eq!(replacement.slot, handle.id.slot);
                    assert_ne!(replacement.generation, handle.id.generation);
                    assert!(
                        shared
                            .native_states
                            .borrow()
                            .get::<DropProbe>(handle.id)
                            .is_none()
                    );
                    assert_eq!(state.value, 41);
                })
                .unwrap();
                assert_eq!(drops.get(), 1);
                assert_eq!(
                    cx.with_native_state(&handle, |_| ()).unwrap_err(),
                    JsError::Runtime(RuntimeError::StaleHandle)
                );
            })
            .unwrap();
        runtime.invalidate().unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn publication_error_is_captured_before_state_destruction() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let drops = Rc::new(Cell::new(0));
        runtime.with_context(|cx| {
            let observed_drops = Rc::clone(&drops);
            cx.install_host_function("stateWasDropped", move |_| {
                Ok(super::super::Value::Boolean(observed_drops.get() != 0))
            }).unwrap();
            cx.eval("Object.defineProperty(globalThis, 'rejectState', { set(value) { globalThis.savedRejectedState = value; throw { toString() { return stateWasDropped() ? 'changed after cleanup' : 'publication failed'; } }; } })", "native-setter.js").unwrap();
            let error = cx.install_native_state("rejectState", DropProbe {
                value: 42,
                drops: Rc::clone(&drops),
            }).unwrap_err();
            assert!(matches!(error, JsError::Exception(ref error) if error.message().contains("publication failed")), "{error}");
            assert_eq!(drops.get(), 1);
            assert_eq!(shared.native_states.borrow().live, 0);
            cx.eval("delete savedRejectedState", "delete-rejected-state.js").unwrap();
            cx.collect_garbage().unwrap();
        }).unwrap();
        runtime.invalidate().unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn operation_panic_releases_lease_without_rolling_back_state() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|cx| {
                let handle = cx.install_native_state("resource", Cell::new(1)).unwrap();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cx.with_native_state(&handle, |state| {
                        state.set(2);
                        panic!("operation failed");
                    })
                    .unwrap();
                }));
                assert!(result.is_err());
                assert_eq!(cx.with_native_state(&handle, Cell::get).unwrap(), 2);
                let registry = cx.shared.native_states.borrow();
                assert_eq!(
                    Rc::strong_count(registry.slots[handle.id.slot].value.as_ref().unwrap()),
                    1
                );
            })
            .unwrap();
    }

    struct PanicDropProbe {
        shared: Weak<Shared>,
        drops: Rc<Cell<usize>>,
    }

    impl Drop for PanicDropProbe {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
            assert!(
                self.shared
                    .upgrade()
                    .unwrap()
                    .native_states
                    .try_borrow_mut()
                    .is_ok()
            );
            panic!("state destructor failed");
        }
    }

    #[test]
    fn retiring_during_unwind_contains_last_lease_destructor_panic() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let drops = Rc::new(Cell::new(0));
        runtime
            .with_context(|cx| {
                let handle = cx
                    .install_native_state(
                        "resource",
                        PanicDropProbe {
                            shared: Rc::downgrade(&shared),
                            drops: Rc::clone(&drops),
                        },
                    )
                    .unwrap();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cx.with_native_state(&handle, |_| {
                        let removed = shared.native_states.borrow_mut().remove(handle.id);
                        drop_state(&shared, removed);
                        assert_eq!(drops.get(), 0);
                        panic!("operation failed");
                    })
                    .unwrap();
                }));
                let payload = result.expect_err("operation must still unwind");
                assert_eq!(payload.downcast_ref::<&str>(), Some(&"operation failed"));
                assert_eq!(drops.get(), 1);
                assert_eq!(shared.native_drop_panics.get(), 1);
                let value = cx.eval("42", "after-unwind.js").unwrap();
                assert!((cx.number(&value).unwrap() - 42.0).abs() < f64::EPSILON);
            })
            .unwrap();
        runtime.invalidate().unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn queued_finalizer_retirement_preserves_the_active_operation() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let drops = Rc::new(Cell::new(0));
        runtime
            .with_context(|cx| {
                let handle = cx
                    .install_native_state(
                        "resource",
                        DropProbe {
                            value: 42,
                            drops: Rc::clone(&drops),
                        },
                    )
                    .unwrap();
                cx.with_native_state(&handle, |state| {
                    // Inject a finalizer signal on a worker to make retirement timing
                    // deterministic. No engine API or application state moves there.
                    let token = Box::new(FinalizerToken {
                        queue: Arc::clone(&shared.native_finalizers),
                        id: handle.id,
                        next: AtomicPtr::new(ptr::null_mut()),
                    });
                    std::thread::spawn(move || {
                        let queue = Arc::clone(&token.queue);
                        // SAFETY: This thread transfers its uniquely owned test token
                        // to the queue, exactly as the engine finalizer does.
                        unsafe { queue.push(Box::into_raw(token)) };
                    })
                    .join()
                    .unwrap();
                    shared.drain_native_finalizers();
                    assert_eq!(shared.native_states.borrow().live, 0);
                    assert_eq!(drops.get(), 0);
                    assert_eq!(state.value, 42);
                })
                .unwrap();
                assert_eq!(drops.get(), 1);
            })
            .unwrap();
        // The real wrapper's later signal must be stale and harmless.
        runtime.invalidate().unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn access_rejects_wrong_type_and_draining_runtime() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let handle = cx.install_native_state("resource", 42_u32).unwrap();
                let forged = NativeObject::<String> {
                    runtime: Rc::downgrade(&shared),
                    id: handle.id,
                    _type: PhantomData,
                    _affine: PhantomData,
                };
                assert_eq!(
                    cx.with_native_state(&forged, |_| panic!("wrong type ran"))
                        .unwrap_err(),
                    JsError::Runtime(RuntimeError::StaleHandle)
                );
                shared.gate.request_drain();
                assert_eq!(
                    cx.with_native_state(&handle, |_| panic!("draining access ran"))
                        .unwrap_err(),
                    JsError::Runtime(RuntimeError::Invalidated)
                );
            })
            .unwrap();
        runtime.invalidate().unwrap();
    }

    #[test]
    fn last_native_lease_defers_its_persistent_root_to_host_maintenance() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let value = cx.eval("({})", "state-owned-root.js").unwrap();
                let root = cx.persist(&value).unwrap();
                let handle = cx.install_native_state("resource", root).unwrap();
                cx.with_native_state(&handle, |_| {
                    let removed = shared.native_states.borrow_mut().remove(handle.id);
                    drop_state(&shared, removed);
                    assert!(shared.roots.borrow().pending_head.is_none());
                })
                .unwrap();
                assert!(shared.roots.borrow().pending_head.is_some());
            })
            .unwrap();
        assert!(shared.roots.borrow().pending_head.is_none());
        assert!(
            shared
                .roots
                .borrow()
                .slots
                .iter()
                .all(|slot| slot.value.is_none())
        );
    }

    #[test]
    fn class_definition_matches_64_bit_jsc_layout() {
        assert_eq!(size_of::<sys::ClassDefinition>(), 128);
        assert_eq!(align_of::<sys::ClassDefinition>(), 8);
    }
}
