// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host lifecycle, thread entry, and scheduling for `RustJSI`.

#![forbid(unsafe_code)]

mod contract;
mod entry;
mod identity;

pub use contract::Host;
pub use entry::{
    CleanupGuard, EntryGate, EntryGuard, FinalEntryOutcome, FinalEntryPolicy, GateError, HostState,
};
pub use identity::{AttachmentEpoch, AttachmentId, IdentityError, RuntimeId, RuntimeIdentity};
