// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime state, roots, tasks, resources, and diagnostics for `RustJSI`.

#![forbid(unsafe_code)]

mod closable_mailbox;
mod drain;
mod ingress;
mod mailbox;

pub use closable_mailbox::{
    ClosableMailbox, ClosableMailboxAcquire, ClosableMailboxDrain, ClosableMailboxEnqueueError,
    ClosableMailboxPostError, ClosableMailboxState, TerminalAcquireError, TerminalDrainFinishError,
    TerminalMailboxDrain,
};
pub use drain::{
    DrainAcquire, DrainAfter, DrainClose, DrainPermit, DrainRequest, DrainSignal, DrainState,
};
pub use ingress::{
    IngressClose, IngressGate, IngressPermit, IngressReserveError, IngressSealError, IngressState,
};
pub use mailbox::{
    BoundedMailbox, MailboxAcquire, MailboxDrain, MailboxEnqueue, MailboxEnqueueError,
};
