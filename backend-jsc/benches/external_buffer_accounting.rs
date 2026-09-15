// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exact-owner external-buffer probe for the direct `JavaScriptCore` route.

#[cfg(target_os = "macos")]
#[global_allocator]
static COUNTING_ALLOCATOR: allocation::CountingAllocator = allocation::CountingAllocator;

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::Runtime;

    const DEFAULT_BYTES: usize = 1024 * 1024;
    const MAX_GC_PASSES: usize = 32;

    let byte_len = std::env::var("RUSTJSI_EXTERNAL_BUFFER_BYTES").map_or(DEFAULT_BYTES, |value| {
        value
            .parse::<usize>()
            .expect("RUSTJSI_EXTERNAL_BUFFER_BYTES must be a usize")
    });
    let expected_first_byte = first_byte(byte_len);
    let mut runtime = Runtime::new().expect("create external-buffer probe runtime");
    let (buffer, rust_transfer_delta) = runtime
        .with_context(|context| {
            let owner = payload(byte_len);
            let before_transfer = allocation::snapshot();
            let buffer = context
                .install_external_buffer(
                    "__rustjsi_external_buffer_probe",
                    owner,
                )
                .expect("transfer exact external buffer");
            let rust_transfer_delta = allocation::snapshot().difference(before_transfer);
            let result = context
                .eval(
                    "(() => {\
                         const bytes = new Uint8Array(globalThis.__rustjsi_external_buffer_probe);\
                         if (bytes.length !== globalThis.__rustjsi_external_buffer_probe.byteLength) {\
                           throw new Error('unexpected buffer length');\
                         }\
                         if (bytes.length === 0) return -1;\
                         bytes[0] ^= 0xff;\
                         return bytes[0];\
                       })()",
                    "external-buffer-accounting.js",
                )
                .expect("mutate transferred buffer from JavaScript");
            let observed = context.number(&result).expect("read mutation result");
            let expected = byte_len
                .checked_sub(1)
                .map_or(-1.0, |_| f64::from(expected_first_byte ^ 0xff));
            assert_eq!(observed.to_bits(), expected.to_bits(), "wrong buffer mutation");
            context
                .eval(
                    "delete globalThis.__rustjsi_external_buffer_probe",
                    "external-buffer-accounting-delete.js",
                )
                .expect("remove JavaScript buffer reachability");
            (buffer, rust_transfer_delta)
        })
        .expect("enter external-buffer probe runtime");

    collect_until_deallocated(&mut runtime, &buffer, MAX_GC_PASSES);
    let origin = buffer.backing_store_matches_origin();
    let deallocator_origin = buffer.deallocator_received_origin();
    let deallocator_runtime_thread = buffer.deallocator_ran_on_runtime_thread();
    assert_eq!(origin, (byte_len != 0).then_some(true));
    assert_eq!(deallocator_origin, Some(true));

    println!(
        "external_buffer_accounting: payload_bytes={byte_len} \
         backing_origin={origin:?} js_mutation=verified \
         deallocator_origin={deallocator_origin:?} \
         deallocator_runtime_thread={deallocator_runtime_thread:?} \
         rust_transfer_allocations={} rust_transfer_allocated_bytes={} \
         rust_transfer_deallocations={} rust_transfer_deallocated_bytes={}",
        rust_transfer_delta.allocations,
        rust_transfer_delta.allocated_bytes,
        rust_transfer_delta.deallocations,
        rust_transfer_delta.deallocated_bytes,
    );
}

#[cfg(target_os = "macos")]
fn payload(byte_len: usize) -> Box<[u8]> {
    let mut bytes = vec![0; byte_len].into_boxed_slice();
    if let Some(first) = bytes.first_mut() {
        *first = first_byte(byte_len);
    }
    bytes
}

#[cfg(target_os = "macos")]
const fn first_byte(byte_len: usize) -> u8 {
    if byte_len == 0 { 0 } else { 0x3c }
}

#[cfg(target_os = "macos")]
fn collect_until_deallocated(
    runtime: &mut rustjsi_backend_jsc::Runtime,
    buffer: &rustjsi_backend_jsc::ExternalBuffer,
    max_passes: usize,
) {
    for _ in 0..max_passes {
        if buffer.is_deallocated() {
            return;
        }
        runtime
            .with_context(|context| {
                context.collect_garbage().expect("collect external buffer");
                context
                    .eval(
                        "Array.from({ length: 4096 }, (_, index) => ({ index }))",
                        "external-buffer-accounting-gc-churn.js",
                    )
                    .expect("run external-buffer collection churn");
            })
            .expect("enter external-buffer collection runtime");
    }
    panic!("external buffer owner was not deallocated");
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
    // layout. Counters observe Rust allocator traffic without changing ownership.
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

#[cfg(not(target_os = "macos"))]
fn main() {}
