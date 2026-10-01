// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exact-owner external-buffer probe for the direct `JavaScriptCore` route.

#[cfg(target_os = "macos")]
#[path = "support/allocation.rs"]
mod allocation;

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

#[cfg(not(target_os = "macos"))]
fn main() {}
