// SPDX-License-Identifier: MIT OR Apache-2.0

use std::num::NonZeroUsize;

use rustjsi_backend::BackendFamily;
use rustjsi_runtime::{
    ClosableMailbox, ClosableMailboxAcquire, ClosableMailboxDrain, ClosableMailboxEnqueueError,
    ClosableMailboxPostError, DrainAfter, IngressClose, MailboxEnqueue, TerminalAcquireError,
    TerminalDrainFinishError, TerminalMailboxDrain,
};

use crate::DrainPoster;
use crate::{AttachmentId, Host, ScheduledWork, WorkDispatchError};

/// Attachment-bound host work retained in a close-aware bounded mailbox.
#[derive(Debug)]
pub struct ScheduledWorkMailbox<T> {
    attachment: AttachmentId,
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

/// Failure to enqueue and post attachment-bound work.
#[derive(Debug)]
#[must_use]
pub enum ScheduledWorkPostError<T, E> {
    /// Queue admission rejected and returned the original work record.
    Enqueue(ScheduledWorkEnqueueError<T>),
    /// Posting failed after work was retained in the pending mailbox.
    Post {
        /// Attachment requested for the future drain.
        attachment: AttachmentId,
        /// Host-specific posting failure.
        error: E,
    },
}

/// Failure to post the successor required after a normal work drain finishes.
#[derive(Debug)]
#[must_use]
pub enum ScheduledWorkFinishError<E> {
    /// Work remains pending after the drain, but the host rejected its post.
    Post {
        /// Immutable mailbox attachment requested for the successor drain.
        attachment: AttachmentId,
        /// Host-specific posting failure.
        error: E,
    },
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
    attachment: AttachmentId,
    drain: ClosableMailboxDrain<'mailbox, ScheduledWork<T>>,
}

/// Terminal ownership for residual attachment-bound work.
#[derive(Debug)]
#[must_use]
pub struct TerminalScheduledWorkDrain<'mailbox, T> {
    drain: TerminalMailboxDrain<'mailbox, ScheduledWork<T>>,
}

impl<T> ScheduledWorkMailbox<T> {
    /// Creates a fixed-capacity mailbox for exactly one attachment epoch.
    #[must_use]
    pub fn new(attachment: AttachmentId, capacity: NonZeroUsize) -> Self {
        Self {
            attachment,
            mailbox: ClosableMailbox::new(capacity),
        }
    }

    /// Returns the immutable attachment this mailbox targets.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.attachment
    }

    /// Enqueues payload captured for this mailbox's attachment epoch.
    ///
    /// # Errors
    ///
    /// Returns the complete attachment-bound work record when the mailbox is
    /// full or producer close has started.
    pub fn enqueue(&self, payload: T) -> Result<MailboxEnqueue, ScheduledWorkEnqueueError<T>> {
        self.mailbox
            .enqueue(ScheduledWork::new(self.attachment, payload))
            .map_err(|error| match error {
                ClosableMailboxEnqueueError::Full(work) => ScheduledWorkEnqueueError::Full(work),
                ClosableMailboxEnqueueError::Closed(work) => {
                    ScheduledWorkEnqueueError::Closed(work)
                }
            })
    }

    /// Enqueues work and posts only when the mailbox creates a new drain obligation.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduledWorkPostError::Enqueue`] with the original work on
    /// admission failure. A post failure retains the work in this mailbox; the
    /// caller must use [`Self::post_pending`] or another explicit retry policy.
    pub fn enqueue_and_post<P>(
        &self,
        poster: &P,
        payload: T,
    ) -> Result<MailboxEnqueue, ScheduledWorkPostError<T, P::Error>>
    where
        P: DrainPoster,
    {
        self.mailbox
            .enqueue_and_post(ScheduledWork::new(self.attachment, payload), || {
                poster.post_drain(self.attachment)
            })
            .map_err(|error| match error {
                ClosableMailboxPostError::Enqueue(ClosableMailboxEnqueueError::Full(work)) => {
                    ScheduledWorkPostError::Enqueue(ScheduledWorkEnqueueError::Full(work))
                }
                ClosableMailboxPostError::Enqueue(ClosableMailboxEnqueueError::Closed(work)) => {
                    ScheduledWorkPostError::Enqueue(ScheduledWorkEnqueueError::Closed(work))
                }
                ClosableMailboxPostError::Post(error) => ScheduledWorkPostError::Post {
                    attachment: self.attachment,
                    error,
                },
            })
    }

    /// Posts a retry only while the mailbox still has a pending normal drain.
    ///
    /// # Errors
    ///
    /// Returns the host poster's error while preserving the pending mailbox.
    pub fn post_pending<P>(&self, poster: &P) -> Result<bool, P::Error>
    where
        P: DrainPoster,
    {
        if self.mailbox.is_drain_pending() {
            poster.post_drain(self.attachment)?;
            Ok(true)
        } else {
            Ok(false)
        }
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
                ScheduledWorkAcquire::Acquired(ScheduledWorkDrain {
                    attachment: self.attachment,
                    drain,
                })
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

    /// Dispatches at most one queued record through one legal host entry.
    ///
    /// A successful call with retained work enters the host exactly once and
    /// returns that operation's result. An empty drain does not enter the host.
    /// Rejected work remains in the returned error for caller-owned retry or
    /// terminal policy; this helper does not silently requeue, discard, or
    /// execute another record.
    ///
    /// # Errors
    ///
    /// Returns attachment or host-entry rejection with the consumed queue
    /// record still owned by the caller.
    pub fn dispatch_next<H, R>(
        &self,
        host: &mut H,
        operation: impl for<'entry> FnOnce(&mut <H::Family as BackendFamily>::Backend<'entry>, T) -> R,
    ) -> Result<Option<R>, WorkDispatchError<T, H::Error>>
    where
        H: Host,
    {
        let Some(work) = self.pop() else {
            return Ok(None);
        };
        work.dispatch(host, operation).map(Some)
    }

    /// Finishes normal draining and reports the successor-post obligation.
    pub fn finish(self) -> DrainAfter {
        self.drain.finish()
    }

    /// Finishes normal draining and posts one successor when work arrived during it.
    ///
    /// A successor post is attempted only after [`DrainAfter::Pending`]. Its
    /// attachment is the immutable identity captured when the mailbox was
    /// created. The post runs before normal drain admission releases, so
    /// terminal close cannot race between the pending transition and post.
    /// `Idle` and `Closed` finish outcomes do not call the poster.
    ///
    /// # Errors
    ///
    /// A post failure leaves the mailbox pending. The caller can use
    /// [`ScheduledWorkMailbox::post_pending`] for an explicit retry.
    pub fn finish_and_post<P>(
        self,
        poster: &P,
    ) -> Result<DrainAfter, ScheduledWorkFinishError<P::Error>>
    where
        P: DrainPoster,
    {
        let attachment = self.attachment;
        self.drain.finish_with(|after| {
            if after == DrainAfter::Pending {
                poster
                    .post_drain(attachment)
                    .map_err(|error| ScheduledWorkFinishError::Post { attachment, error })?;
            }
            Ok(after)
        })
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
