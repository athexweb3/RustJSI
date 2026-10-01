// SPDX-License-Identifier: MIT OR Apache-2.0

use std::num::NonZeroUsize;

use rustjsi_runtime::{
    ClosableMailbox, ClosableMailboxAcquire, ClosableMailboxDrain, ClosableMailboxEnqueueError,
    IngressClose, MailboxEnqueue, TerminalAcquireError, TerminalDrainFinishError,
    TerminalMailboxDrain,
};

use crate::{AttachmentId, ScheduledWork};

/// Attachment-bound host work retained in a close-aware bounded mailbox.
#[derive(Debug)]
pub struct ScheduledWorkMailbox<T> {
    mailbox: ClosableMailbox<ScheduledWork<T>>,
}

/// Failure to enqueue attachment-bound work.
#[derive(Debug)]
#[must_use]
pub enum ScheduledWorkEnqueueError<T> {
    /// The mailbox reached fixed capacity.
    Full(ScheduledWork<T>),
    /// Close has stopped producer admission.
    Closed(ScheduledWork<T>),
}

/// Result of acquiring normal scheduled work.
#[derive(Debug)]
#[must_use]
pub enum ScheduledWorkAcquire<'mailbox, T> {
    /// No work is pending.
    Idle,
    /// One normal drain owns queued work.
    Acquired(ScheduledWorkDrain<'mailbox, T>),
    /// A different consumer owns the normal drain.
    Busy,
    /// Terminal close prevents normal draining.
    Unavailable,
}

/// Normal-drain ownership for [`ScheduledWorkMailbox`].
#[derive(Debug)]
#[must_use]
pub struct ScheduledWorkDrain<'mailbox, T> {
    drain: ClosableMailboxDrain<'mailbox, ScheduledWork<T>>,
}

/// Terminal ownership for residual attachment-bound work.
#[derive(Debug)]
#[must_use]
pub struct TerminalScheduledWorkDrain<'mailbox, T> {
    drain: TerminalMailboxDrain<'mailbox, ScheduledWork<T>>,
}

impl<T> ScheduledWorkMailbox<T> {
    /// Creates a fixed-capacity attachment-bound work mailbox.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            mailbox: ClosableMailbox::new(capacity),
        }
    }

    /// Enqueues payload captured for one attachment epoch.
    ///
    /// # Errors
    ///
    /// Returns the complete attachment-bound work record when the mailbox is
    /// full or producer close has started.
    pub fn enqueue(
        &self,
        attachment: AttachmentId,
        payload: T,
    ) -> Result<MailboxEnqueue, ScheduledWorkEnqueueError<T>> {
        self.mailbox
            .enqueue(ScheduledWork::new(attachment, payload))
            .map_err(|error| match error {
                ClosableMailboxEnqueueError::Full(work) => ScheduledWorkEnqueueError::Full(work),
                ClosableMailboxEnqueueError::Closed(work) => {
                    ScheduledWorkEnqueueError::Closed(work)
                }
            })
    }

    /// Stops later producer admission without waiting for earlier publishers.
    pub fn begin_close(&self) -> IngressClose {
        self.mailbox.begin_close()
    }

    /// Acquires normal queued work without entering a backend.
    pub fn acquire(&self) -> ScheduledWorkAcquire<'_, T> {
        match self.mailbox.acquire() {
            ClosableMailboxAcquire::Idle => ScheduledWorkAcquire::Idle,
            ClosableMailboxAcquire::Busy => ScheduledWorkAcquire::Busy,
            ClosableMailboxAcquire::Unavailable(_) => ScheduledWorkAcquire::Unavailable,
            ClosableMailboxAcquire::Acquired(drain) => {
                ScheduledWorkAcquire::Acquired(ScheduledWorkDrain { drain })
            }
        }
    }

    /// Attempts terminal ownership after normal work and producer ingress settle.
    ///
    /// # Errors
    ///
    /// Returns typed pending ownership state from the underlying mailbox.
    pub fn try_begin_terminal_drain(
        &self,
    ) -> Result<TerminalScheduledWorkDrain<'_, T>, TerminalAcquireError> {
        self.mailbox
            .try_begin_terminal_drain()
            .map(|drain| TerminalScheduledWorkDrain { drain })
    }
}

impl<T> ScheduledWorkDrain<'_, T> {
    /// Removes the next attachment-bound work record without entering a backend.
    #[must_use]
    pub fn pop(&self) -> Option<ScheduledWork<T>> {
        self.drain.pop()
    }

    /// Finishes normal draining and reports the successor-post obligation.
    pub fn finish(self) -> rustjsi_runtime::DrainAfter {
        self.drain.finish()
    }
}

impl<T> TerminalScheduledWorkDrain<'_, T> {
    /// Removes one residual work record with terminal caller ownership.
    #[must_use]
    pub fn pop(&self) -> Option<ScheduledWork<T>> {
        self.drain.pop()
    }

    /// Finishes terminal ownership after every residual record transferred.
    ///
    /// # Errors
    ///
    /// Returns typed residual ownership information instead of dropping work.
    pub fn finish(self) -> Result<(), TerminalDrainFinishError> {
        self.drain.finish()
    }
}
