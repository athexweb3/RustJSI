// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime state, roots, tasks, resources, and diagnostics for `RustJSI`.

#![forbid(unsafe_code)]

mod drain;
mod ingress;
mod mailbox;

pub use drain::{
    DrainAcquire, DrainAfter, DrainClose, DrainPermit, DrainRequest, DrainSignal, DrainState,
};
pub use ingress::{
    IngressClose, IngressGate, IngressPermit, IngressReserveError, IngressSealError, IngressState,
};
pub use mailbox::{
    BoundedMailbox, MailboxAcquire, MailboxDrain, MailboxEnqueue, MailboxEnqueueError,
};
