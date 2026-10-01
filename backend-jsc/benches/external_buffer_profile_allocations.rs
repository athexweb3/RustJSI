// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rust allocator probe for the matched owned-external-buffer profile.

#[cfg(target_os = "macos")]
#[path = "support/allocation.rs"]
mod allocation;

#[cfg(target_os = "macos")]
#[path = "support/external_buffers.rs"]
mod external_buffers;

#[cfg(target_os = "macos")]
#[global_allocator]
static COUNTING_ALLOCATOR: allocation::CountingAllocator = allocation::CountingAllocator;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let payload_bytes =
        std::env::var("RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES").map_or(4 * 1024, |value| {
            value
                .parse::<usize>()
                .expect("RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES must be a usize")
        });
    let direct = direct::measure(payload_bytes);
    let rustjsi = rustjsi::measure(payload_bytes);

    println!(
        "external_buffer_profile_allocations: payload_bytes={payload_bytes} \
         block_size={} cleanup=property-clear,runtime-teardown \
         direct_allocations={} direct_allocated_bytes={} \
         direct_deallocations={} direct_deallocated_bytes={} \
         rustjsi_allocations={} rustjsi_allocated_bytes={} \
         rustjsi_deallocations={} rustjsi_deallocated_bytes={}",
        external_buffers::BLOCK_SIZE,
        direct.allocations,
        direct.allocated_bytes,
        direct.deallocations,
        direct.deallocated_bytes,
        rustjsi.allocations,
        rustjsi.allocated_bytes,
        rustjsi.deallocations,
        rustjsi.deallocated_bytes,
    );
}

#[cfg(target_os = "macos")]
mod direct {
    use super::{allocation, external_buffers};

    pub(super) fn measure(payload_bytes: usize) -> allocation::Snapshot {
        let mut context = external_buffers::direct::ContextOwner::new();
        let owners = external_buffers::payloads(payload_bytes);
        let before = allocation::snapshot();
        context.publish_block(owners);
        let observed = allocation::snapshot().difference(before);
        context.clear_properties();
        context.shutdown();
        observed
    }
}

#[cfg(target_os = "macos")]
mod rustjsi {
    use super::{allocation, external_buffers};
    use rustjsi_backend_jsc::Runtime;

    pub(super) fn measure(payload_bytes: usize) -> allocation::Snapshot {
        let mut runtime = Runtime::new().expect("create RustJSI external-buffer runtime");
        let owners = external_buffers::payloads(payload_bytes);
        let mut buffers = Vec::with_capacity(external_buffers::BLOCK_SIZE);
        let before = allocation::snapshot();
        external_buffers::rustjsi::publish_block(&mut runtime, owners, &mut buffers);
        let observed = allocation::snapshot().difference(before);
        external_buffers::rustjsi::clear_properties(&mut runtime);
        external_buffers::rustjsi::shutdown(&mut runtime, &buffers);
        observed
    }
}
