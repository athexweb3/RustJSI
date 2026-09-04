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
    value: Option<Box<dyn Any>>,
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
            entry.value = Some(Box::new(value));
            return Ok(NativeId {
                slot,
                generation: entry.generation,
            });
        }

        let slot = self.slots.len();
        self.slots.push(NativeSlot {
            generation: 1,
            type_id: Some(TypeId::of::<T>()),
            value: Some(Box::new(value)),
        });
        Ok(NativeId {
            slot,
            generation: 1,
        })
    }

    fn get<T: 'static>(&self, id: NativeId) -> Option<&T> {
        let slot = self.slots.get(id.slot)?;
        if slot.generation != id.generation || slot.type_id != Some(TypeId::of::<T>()) {
            return None;
        }
        slot.value.as_ref()?.downcast_ref()
    }

    fn remove(&mut self, id: NativeId) -> Option<Box<dyn Any>> {
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

    pub(super) fn drain(&mut self) -> Vec<Box<dyn Any>> {
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
                drop_state(self.shared, Some(Box::new(state)));
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
            self.rollback_native_object(object, token, id);
            return Err(JsError::Exception(super::exception_to_owned(
                self.raw, exception,
            )));
        }

        Ok(NativeObject {
            runtime: Rc::downgrade(self.shared),
            id,
            _type: PhantomData,
            _affine: PhantomData,
        })
    }

    /// Borrows typed Rust state while its JavaScript wrapper remains live.
    ///
    /// # Errors
    ///
    /// Returns an error for a dead, foreign, collected, or type-invalid handle.
    pub fn with_native_state<T: 'static, R>(
        &mut self,
        handle: &NativeObject<T>,
        operation: impl FnOnce(&T) -> R,
    ) -> Result<R, JsError> {
        let runtime = handle
            .runtime
            .upgrade()
            .ok_or(JsError::Runtime(RuntimeError::Invalidated))?;
        if !Rc::ptr_eq(&runtime, self.shared) {
            return Err(JsError::Runtime(RuntimeError::WrongRuntime));
        }

        let states = self.shared.native_states.borrow();
        let state = states
            .get(handle.id)
            .ok_or(JsError::Runtime(RuntimeError::StaleHandle))?;
        Ok(operation(state))
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

pub(super) fn reclaim_finalized(shared: &Shared, mut token: *mut FinalizerToken) {
    while let Some(current) = NonNull::new(token) {
        // SAFETY: The queue transferred unique ownership of this detached list to the
        // runtime thread. `next` was initialized before publication.
        let boxed = unsafe { Box::from_raw(current.as_ptr()) };
        token = boxed.next.load(Ordering::Relaxed);
        let state = shared.native_states.borrow_mut().remove(boxed.id);
        drop(boxed);
        drop_state(shared, state);
    }
}

pub(super) fn drop_states(shared: &Shared, states: Vec<Box<dyn Any>>) {
    for state in states {
        drop_state(shared, Some(state));
    }
}

fn drop_state(shared: &Shared, state: Option<Box<dyn Any>>) {
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
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn class_definition_matches_64_bit_jsc_layout() {
        assert_eq!(size_of::<sys::ClassDefinition>(), 128);
        assert_eq!(align_of::<sys::ClassDefinition>(), 8);
    }
}
