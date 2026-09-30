// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime state, roots, tasks, resources, and diagnostics for `RustJSI`.

#![forbid(unsafe_code)]

mod drain;
mod mailbox;

pub use drain::{
    DrainAcquire, DrainAfter, DrainClose, DrainPermit, DrainRequest, DrainSignal, DrainState,
};
pub use mailbox::{
    BoundedMailbox, MailboxAcquire, MailboxDrain, MailboxEnqueue, MailboxEnqueueError,
};
