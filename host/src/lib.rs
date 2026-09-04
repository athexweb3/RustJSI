// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host lifecycle, thread entry, and scheduling for `RustJSI`.

#![forbid(unsafe_code)]

mod entry;

pub use entry::{
    CleanupGuard, EntryGate, EntryGuard, FinalEntryOutcome, FinalEntryPolicy, GateError, HostState,
};
