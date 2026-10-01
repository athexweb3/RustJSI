// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rust allocator observation for isolated benchmark probes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

pub(crate) struct CountingAllocator;

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
pub(crate) struct Snapshot {
    pub(crate) allocations: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) deallocations: u64,
    pub(crate) deallocated_bytes: u64,
}

impl Snapshot {
    pub(crate) fn difference(self, earlier: Self) -> Self {
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

pub(crate) fn snapshot() -> Snapshot {
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
