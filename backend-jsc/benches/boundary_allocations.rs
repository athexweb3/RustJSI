// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rust allocator probe for the empty host-entry boundary paths.

#[cfg(target_os = "macos")]
#[global_allocator]
static COUNTING_ALLOCATOR: allocation::CountingAllocator = allocation::CountingAllocator;

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{Attachment, Runtime};
    use rustjsi_host::{EntryGate, FinalEntryPolicy, RuntimeIdentity};
    use std::hint::black_box;
    use std::num::NonZeroU32;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 1_000_000;

    let gate = EntryGate::new(
        NonZeroU32::new(64).expect("nonzero entry limit"),
        FinalEntryPolicy::Unavailable,
    );
    let gate_allocations = measure(WARMUP, ITERATIONS, || {
        let entry = black_box(&gate).try_enter().expect("admit host entry");
        black_box(&entry);
        drop(entry);
    });

    let mut runtime = Runtime::new().expect("create RustJSI JSC runtime");
    let common_allocations = measure(WARMUP, ITERATIONS, || {
        black_box(&mut runtime)
            .with_backend(|_| black_box(()))
            .expect("enter common backend");
    });

    let foreign_owner = raw::OwnedContext::new();
    let mut identity = RuntimeIdentity::allocate().expect("allocate foreign host identity");
    let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed)
        .expect("create foreign attachment");
    let foreign_allocations = measure(WARMUP, ITERATIONS, || {
        // SAFETY: The benchmark owner keeps this context live on the current
        // thread and lends the same global context to every entry.
        unsafe {
            black_box(&mut attachment)
                .with_backend(foreign_owner.as_void(), |_| black_box(()))
                .expect("enter foreign common backend");
        }
    });

    print_measurement("host_gate_admit_and_exit", gate_allocations, ITERATIONS);
    print_measurement("jsc_common_empty_entry", common_allocations, ITERATIONS);
    print_measurement(
        "jsc_foreign_common_empty_entry",
        foreign_allocations,
        ITERATIONS,
    );

    // SAFETY: This is the same still-live context used for every measured entry.
    let _ = unsafe { attachment.detach_with_context(foreign_owner.as_void()) }
        .expect("detach foreign benchmark attachment");
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn measure(warmup: u32, iterations: u32, mut operation: impl FnMut()) -> allocation::Snapshot {
    for _ in 0..warmup {
        operation();
    }
    let before = allocation::snapshot();
    for _ in 0..iterations {
        operation();
    }
    allocation::snapshot().difference(before)
}

#[cfg(target_os = "macos")]
fn print_measurement(name: &str, measurement: allocation::Snapshot, iterations: u32) {
    println!(
        "rust_alloc_{name}: {} calls {} bytes {} deallocations {} deallocated-bytes ({iterations} iterations)",
        measurement.allocations,
        measurement.allocated_bytes,
        measurement.deallocations,
        measurement.deallocated_bytes,
    );
}

#[cfg(target_os = "macos")]
mod allocation {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};

    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
    static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static DEALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

    pub(super) struct CountingAllocator;

    // SAFETY: Every operation delegates to `System` with the original pointer and
    // layout. The counters are observational and do not affect allocator state.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: The caller's allocation contract is forwarded to `System`.
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: The caller's allocation contract is forwarded to `System`.
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            record_deallocation(layout.size());
            // SAFETY: The caller's matching pointer and layout are forwarded.
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            // SAFETY: The live allocation, layout and requested size are forwarded.
            let replacement = unsafe { System.realloc(pointer, layout, size) };
            if !replacement.is_null() {
                record_deallocation(layout.size());
                record_allocation(size);
            }
            replacement
        }
    }

    #[derive(Clone, Copy)]
    pub(super) struct Snapshot {
        pub(super) allocations: u64,
        pub(super) allocated_bytes: u64,
        pub(super) deallocations: u64,
        pub(super) deallocated_bytes: u64,
    }

    impl Snapshot {
        pub(super) fn difference(self, earlier: Self) -> Self {
            Self {
                allocations: self.allocations.wrapping_sub(earlier.allocations),
                allocated_bytes: self.allocated_bytes.wrapping_sub(earlier.allocated_bytes),
                deallocations: self.deallocations.wrapping_sub(earlier.deallocations),
                deallocated_bytes: self
                    .deallocated_bytes
                    .wrapping_sub(earlier.deallocated_bytes),
            }
        }
    }

    pub(super) fn snapshot() -> Snapshot {
        Snapshot {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
            deallocated_bytes: DEALLOCATED_BYTES.load(Ordering::Relaxed),
        }
    }

    fn record_allocation(size: usize) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }

    fn record_deallocation(size: usize) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }
}

#[cfg(target_os = "macos")]
mod raw {
    use std::ffi::c_void;
    use std::ptr;

    type GlobalContext = *mut c_void;

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "JSGlobalContextCreate"]
        fn context_create(class: *mut c_void) -> GlobalContext;

        #[link_name = "JSGlobalContextRelease"]
        fn context_release(context: GlobalContext);
    }

    pub(super) struct OwnedContext(GlobalContext);

    impl OwnedContext {
        pub(super) fn new() -> Self {
            // SAFETY: A null class requests the default global. Check before use.
            let context = unsafe { context_create(ptr::null_mut()) };
            assert!(!context.is_null(), "create allocation-probe JSC context");
            Self(context)
        }

        pub(super) fn as_void(&self) -> *mut c_void {
            self.0.cast()
        }
    }

    impl Drop for OwnedContext {
        fn drop(&mut self) {
            // SAFETY: This guard uniquely owns one successful creation after all
            // borrowed entries and the attachment have ended.
            unsafe { context_release(self.0) };
        }
    }
}
