// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Context, JsError, JsString, Shared, exception_to_owned};
use crate::sys;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, ThreadId};

const MAX_EXTERNAL_ALLOCATIONS: usize = 4_096;
const MAX_EXTERNAL_BYTES: usize = 64 * 1024 * 1024;

/// An observation handle for Rust-owned bytes transferred to `JavaScriptCore`.
///
/// This handle does not keep the JavaScript `ArrayBuffer` alive and never grants
/// Rust access to its mutable backing store.
pub struct ExternalBuffer {
    observation: Arc<ExternalObservation>,
    byte_len: usize,
    backing_store_matches_origin: Option<bool>,
}

pub(super) struct ExternalLedger {
    live_allocations: AtomicUsize,
    live_bytes: AtomicUsize,
    deallocations: AtomicUsize,
}

struct ExternalOwner {
    bytes: Box<[u8]>,
    owner_thread: ThreadId,
    ledger: Arc<ExternalLedger>,
    observation: Arc<ExternalObservation>,
}

pub(super) struct ExternalObservation {
    deallocations: AtomicUsize,
    deallocator_received_origin: AtomicBool,
    deallocator_ran_on_owner: AtomicBool,
}

impl std::fmt::Debug for ExternalBuffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExternalBuffer(..)")
    }
}

impl ExternalBuffer {
    /// Returns the number of transferred payload bytes.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Reports whether JSC returned the original non-empty payload address
    /// immediately after construction.
    ///
    /// An empty payload returns `None` because pointer identity has no payload
    /// significance. A `true` result is evidence for this construction, not a
    /// claim that JavaScript access has no wrapper, GC, or cache cost.
    #[must_use]
    pub fn backing_store_matches_origin(&self) -> Option<bool> {
        self.backing_store_matches_origin
    }

    /// Returns whether JSC has invoked the backing-store deallocator.
    #[must_use]
    pub fn is_deallocated(&self) -> bool {
        self.observation.deallocations.load(Ordering::Acquire) != 0
    }

    /// Returns whether the deallocator received the pointer originally handed to
    /// JSC, once deallocation has occurred.
    #[must_use]
    pub fn deallocator_received_origin(&self) -> Option<bool> {
        self.is_deallocated().then(|| {
            self.observation
                .deallocator_received_origin
                .load(Ordering::Acquire)
        })
    }

    /// Returns whether deallocation occurred on the runtime-owning thread.
    ///
    /// `RustJSI` does not require this to be true; the transferred allocation is
    /// safe to release from any thread.
    #[must_use]
    pub fn deallocator_ran_on_runtime_thread(&self) -> Option<bool> {
        self.is_deallocated().then(|| {
            self.observation
                .deallocator_ran_on_owner
                .load(Ordering::Acquire)
        })
    }
}

impl ExternalLedger {
    pub(super) fn new() -> Self {
        Self {
            live_allocations: AtomicUsize::new(0),
            live_bytes: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
        }
    }

    pub(super) fn reserve(&self, byte_len: usize) -> Result<(), JsError> {
        self.live_allocations
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < MAX_EXTERNAL_ALLOCATIONS).then_some(live + 1)
            })
            .map_err(|_| JsError::Backend("external-buffer allocation quota exceeded"))?;

        if self
            .live_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_add(byte_len)
                    .filter(|total| *total <= MAX_EXTERNAL_BYTES)
            })
            .is_err()
        {
            self.live_allocations.fetch_sub(1, Ordering::AcqRel);
            return Err(JsError::Backend("external-buffer byte quota exceeded"));
        }
        Ok(())
    }

    fn release(&self, byte_len: usize) {
        self.live_bytes.fetch_sub(byte_len, Ordering::AcqRel);
        self.live_allocations.fetch_sub(1, Ordering::AcqRel);
        self.deallocations.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn live_allocations(&self) -> usize {
        self.live_allocations.load(Ordering::Acquire)
    }

    pub(super) fn live_bytes(&self) -> usize {
        self.live_bytes.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn deallocations(&self) -> usize {
        self.deallocations.load(Ordering::Acquire)
    }
}

impl Context<'_> {
    /// Transfers Rust-owned boxed bytes into a global JavaScript `ArrayBuffer`.
    ///
    /// JSC receives the boxed slice's allocation and may mutate it through JavaScript.
    /// Rust retains no reference to the payload after this call. The returned
    /// observation handle does not root the JavaScript wrapper.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, quota, construction, publication, or JavaScript
    /// exception error.
    pub fn install_external_buffer(
        &mut self,
        name: &str,
        bytes: Box<[u8]>,
    ) -> Result<ExternalBuffer, JsError> {
        self.shared.ensure_active().map_err(JsError::Runtime)?;
        let property = JsString::new(name)?;
        self.shared.external_buffers.reserve(bytes.len())?;

        let byte_len = bytes.len();
        let observation = new_observation();
        let (object, backing_store_matches_origin) =
            make_external_object(self.shared, self.raw, bytes, &observation)?;
        self.publish_external_object(&property, object)?;

        Ok(ExternalBuffer {
            observation,
            byte_len,
            backing_store_matches_origin,
        })
    }

    fn publish_external_object(
        &self,
        property: &JsString,
        object: NonNull<sys::OpaqueValue>,
    ) -> Result<(), JsError> {
        // SAFETY: The active context always has a global object.
        let global = unsafe { sys::context_get_global_object(self.raw.as_ptr()) };
        let global = NonNull::new(global).ok_or(JsError::Backend(
            "JavaScriptCore returned a null global object",
        ))?;
        let mut publication_exception = ptr::null();
        // SAFETY: The property, object, and global object are live in this context.
        // JSC retains the external backing through the published ArrayBuffer.
        unsafe {
            sys::object_set_property(
                self.raw.as_ptr(),
                global.as_ptr(),
                property.as_ptr(),
                object.as_ptr(),
                0,
                &raw mut publication_exception,
            );
        }
        if !publication_exception.is_null() {
            return Err(JsError::Exception(exception_to_owned(
                self.raw,
                publication_exception,
            )));
        }
        Ok(())
    }
}

pub(super) fn new_observation() -> Arc<ExternalObservation> {
    Arc::new(ExternalObservation {
        deallocations: AtomicUsize::new(0),
        deallocator_received_origin: AtomicBool::new(false),
        deallocator_ran_on_owner: AtomicBool::new(false),
    })
}

pub(super) fn make_external_object(
    shared: &Shared,
    raw: NonNull<sys::OpaqueContext>,
    mut bytes: Box<[u8]>,
    observation: &Arc<ExternalObservation>,
) -> Result<(NonNull<sys::OpaqueValue>, Option<bool>), JsError> {
    let byte_len = bytes.len();
    let origin = bytes.as_mut_ptr();
    let owner = Box::new(ExternalOwner {
        bytes,
        owner_thread: thread::current().id(),
        ledger: Arc::clone(&shared.external_buffers),
        observation: Arc::clone(observation),
    });
    let owner = Box::into_raw(owner);
    let mut exception = ptr::null();

    // SAFETY: `owner` contains the vector that owns `origin..origin+byte_len`.
    // Ownership of that allocation and the non-null deallocator context is
    // transferred to JSC for both success and exception paths, as required by
    // the public C API contract. No Rust payload reference remains afterwards.
    let object = unsafe {
        sys::object_make_array_buffer_with_bytes_no_copy(
            raw.as_ptr(),
            origin.cast(),
            byte_len,
            Some(external_bytes_deallocator),
            owner.cast(),
            &raw mut exception,
        )
    };
    if !exception.is_null() {
        return Err(JsError::Exception(exception_to_owned(raw, exception)));
    }
    let object = NonNull::new(object).ok_or(JsError::Backend(
        "JavaScriptCore returned a null external ArrayBuffer",
    ))?;

    let mut inspection_exception = ptr::null();
    // SAFETY: `object` is a live ArrayBuffer in this active context. Length is
    // queried before the temporary backing pointer so no JSC call follows while
    // that pointer is being inspected.
    let observed_len = unsafe {
        sys::object_get_array_buffer_byte_length(
            raw.as_ptr(),
            object.as_ptr(),
            &raw mut inspection_exception,
        )
    };
    if !inspection_exception.is_null() {
        return Err(JsError::Exception(exception_to_owned(
            raw,
            inspection_exception,
        )));
    }
    if observed_len != byte_len {
        return Err(JsError::Backend(
            "JavaScriptCore changed the external ArrayBuffer length",
        ));
    }

    // SAFETY: This is the final JSC call before the temporary pointer is compared
    // and discarded. Non-empty backing stores must return a usable pointer.
    let observed_origin = unsafe {
        sys::object_get_array_buffer_bytes_ptr(
            raw.as_ptr(),
            object.as_ptr(),
            &raw mut inspection_exception,
        )
    };
    if !inspection_exception.is_null() {
        return Err(JsError::Exception(exception_to_owned(
            raw,
            inspection_exception,
        )));
    }
    if byte_len != 0 && observed_origin.is_null() {
        return Err(JsError::Backend(
            "JavaScriptCore returned a null external backing store",
        ));
    }
    if byte_len != 0 && observed_origin != origin.cast() {
        return Err(JsError::Backend(
            "JavaScriptCore did not retain the external backing store",
        ));
    }
    let backing_store_matches_origin = (byte_len != 0).then_some(true);

    Ok((object, backing_store_matches_origin))
}

unsafe extern "C" fn external_bytes_deallocator(
    bytes: *mut std::ffi::c_void,
    owner: *mut std::ffi::c_void,
) {
    if owner.is_null() {
        return;
    }
    let _ = super::contain_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: JSC invokes the registered deallocator exactly once with the
        // context whose unique ownership was transferred at construction.
        let mut owner = unsafe { Box::from_raw(owner.cast::<ExternalOwner>()) };
        owner.observation.deallocator_received_origin.store(
            bytes.cast::<u8>() == owner.bytes.as_mut_ptr(),
            Ordering::Release,
        );
        owner.observation.deallocator_ran_on_owner.store(
            thread::current().id() == owner.owner_thread,
            Ordering::Release,
        );
        owner
            .observation
            .deallocations
            .fetch_add(1, Ordering::AcqRel);
        owner.ledger.release(owner.bytes.len());
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_quota_rejection_refunds_reserved_local_capacity() {
        use crate::{RootLimits, Runtime};
        use rustjsi_backend::{
            BackendBase, BackendScope, OwnedExternalBufferScope, OwnershipTransferError,
        };
        let mut runtime = Runtime::new_with_root_limits(RootLimits {
            local_roots: 1,
            ..RootLimits::default()
        })
        .unwrap();
        let ledger = Arc::clone(&runtime.shared.external_buffers);
        // Synthetic ledger saturation, not engine allocations.
        for _ in 0..MAX_EXTERNAL_ALLOCATIONS {
            ledger.reserve(0).unwrap();
        }
        runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                assert!(matches!(
                    scope.externalize(vec![1].into_boxed_slice()),
                    Err(OwnershipTransferError::Rejected { .. })
                ));
                scope.string("local reservation refunded").unwrap();
            })
            .unwrap();
        for _ in 0..MAX_EXTERNAL_ALLOCATIONS {
            ledger.release(0);
        }
        assert_eq!(ledger.live_allocations(), 0);
    }

    #[test]
    fn external_owner_can_be_released_off_thread() {
        let ledger = Arc::new(ExternalLedger::new());
        ledger.reserve(4).unwrap();
        let observation = Arc::new(ExternalObservation {
            deallocations: AtomicUsize::new(0),
            deallocator_received_origin: AtomicBool::new(false),
            deallocator_ran_on_owner: AtomicBool::new(false),
        });
        let mut owner = Box::new(ExternalOwner {
            bytes: vec![1, 2, 3, 4].into_boxed_slice(),
            owner_thread: thread::current().id(),
            ledger: Arc::clone(&ledger),
            observation: Arc::clone(&observation),
        });
        let bytes = owner.bytes.as_mut_ptr() as usize;
        let owner = Box::into_raw(owner) as usize;

        thread::spawn(move || {
            // SAFETY: The test transfers the unique owner and its live byte pointer
            // to the simulated one-shot JSC deallocator invocation.
            unsafe { external_bytes_deallocator(bytes as *mut _, owner as *mut _) };
        })
        .join()
        .unwrap();

        assert_eq!(observation.deallocations.load(Ordering::Acquire), 1);
        assert!(
            observation
                .deallocator_received_origin
                .load(Ordering::Acquire)
        );
        assert!(!observation.deallocator_ran_on_owner.load(Ordering::Acquire));
        assert_eq!(ledger.live_allocations(), 0);
        assert_eq!(ledger.live_bytes(), 0);
        assert_eq!(ledger.deallocations(), 1);
    }

    #[test]
    fn byte_quota_rejection_rolls_back_allocation_reservation() {
        let ledger = ExternalLedger::new();
        assert!(ledger.reserve(MAX_EXTERNAL_BYTES + 1).is_err());
        assert_eq!(ledger.live_allocations(), 0);
        assert_eq!(ledger.live_bytes(), 0);
    }

    #[test]
    fn observation_handle_is_send_and_sync() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<ExternalBuffer>();
    }
}
