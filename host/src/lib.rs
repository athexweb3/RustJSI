// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host lifecycle, thread entry, and scheduling for `RustJSI`.

#![forbid(unsafe_code)]

mod contract;
mod drain_registration;
mod entry;
mod identity;
mod post;
mod work;
mod work_mailbox;
mod work_owner;

pub use contract::Host;
pub use drain_registration::{
    DrainRegistration, DrainRegistrationReplaceError, DrainRegistrationState, DrainTaskResolution,
};
pub use entry::{
    CleanupGuard, EntryGate, EntryGuard, FinalEntryOutcome, FinalEntryPolicy, GateError, HostState,
};
pub use identity::{AttachmentEpoch, AttachmentId, IdentityError, RuntimeId, RuntimeIdentity};
pub use post::DrainPoster;
pub use work::{ScheduledWork, WorkDispatchError};
pub use work_mailbox::{
    ScheduledWorkAcquire, ScheduledWorkDrain, ScheduledWorkEnqueueError, ScheduledWorkFinishError,
    ScheduledWorkMailbox, ScheduledWorkPostError, TerminalScheduledWorkDrain,
};
pub use work_owner::{
    AttachmentWorkOwner, AttachmentWorkReplaceError, AttachmentWorkResolution, ScheduledWorkSender,
};
