// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host lifecycle, thread entry, and scheduling for `RustJSI`.

#![forbid(unsafe_code)]

mod contract;
mod entry;
mod identity;
mod work;
mod work_mailbox;

pub use contract::Host;
pub use entry::{
    CleanupGuard, EntryGate, EntryGuard, FinalEntryOutcome, FinalEntryPolicy, GateError, HostState,
};
pub use identity::{AttachmentEpoch, AttachmentId, IdentityError, RuntimeId, RuntimeIdentity};
pub use work::{ScheduledWork, WorkDispatchError};
pub use work_mailbox::{
    ScheduledWorkAcquire, ScheduledWorkDrain, ScheduledWorkEnqueueError, ScheduledWorkMailbox,
    TerminalScheduledWorkDrain,
};
