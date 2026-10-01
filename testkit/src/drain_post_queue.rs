// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;

use rustjsi_host::{AttachmentId, DrainPoster};
use rustjsi_runtime::{
    BoundedMailbox, DrainAfter, MailboxAcquire, MailboxDrain, MailboxEnqueueError,
};

/// Deterministic fixed-capacity queue of attachment-only drain posts.
///
/// This test fixture accepts the same identity-only posts as [`DrainPoster`].
/// It deliberately has no backend, payload, host entry, lifecycle, or platform
/// wake-up policy. A test driver acquires queued records and decides which host
/// mailbox, if any, may consume a matching attachment.
///
/// Every accepted [`DrainPoster::post_drain`] call retains one record. The
/// fixture does not deduplicate attachments or decide whether an older task is
/// stale; those are host scheduler and lifecycle responsibilities.
#[derive(Debug)]
pub struct DrainPostQueue {
    mailbox: BoundedMailbox<AttachmentId>,
}

/// Fixed-capacity rejection while accepting a drain post.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DrainPostQueueFull {
    attachment: AttachmentId,
}

/// Result of acquiring deterministic drain-post records.
#[derive(Debug)]
#[must_use]
pub enum DrainPostAcquire<'queue> {
    /// No attachment post is pending.
    Idle,
    /// One consumer owns the queued post records.
    Acquired(DrainPostDrain<'queue>),
    /// A different consumer owns the queued post records.
    Busy,
}

/// Affine ownership of one active [`DrainPostQueue`] drain.
#[derive(Debug)]
#[must_use = "finish the drain after consuming the chosen post records"]
pub struct DrainPostDrain<'queue> {
    drain: MailboxDrain<'queue, AttachmentId>,
}

impl DrainPostQueue {
    /// Creates a deterministic queue with a fixed non-zero post capacity.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            mailbox: BoundedMailbox::new(capacity),
        }
    }

    /// Returns the maximum number of retained attachment posts.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.mailbox.capacity()
    }

    /// Returns a concurrent snapshot of retained attachment posts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mailbox.len()
    }

    /// Returns whether this instant contains no retained attachment posts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mailbox.is_empty()
    }

    /// Returns whether another deterministic drain is pending.
    #[must_use]
    pub fn is_drain_pending(&self) -> bool {
        self.mailbox.is_drain_pending()
    }

    /// Acquires queued attachment posts without entering a host or backend.
    #[must_use]
    pub fn acquire(&self) -> DrainPostAcquire<'_> {
        match self.mailbox.acquire() {
            MailboxAcquire::Idle => DrainPostAcquire::Idle,
            MailboxAcquire::Acquired(drain) => DrainPostAcquire::Acquired(DrainPostDrain { drain }),
            MailboxAcquire::Busy => DrainPostAcquire::Busy,
        }
    }
}

impl DrainPostQueueFull {
    /// Returns the attachment whose post could not be retained.
    #[must_use]
    pub const fn attachment_id(self) -> AttachmentId {
        self.attachment
    }
}

impl DrainPoster for DrainPostQueue {
    type Error = DrainPostQueueFull;

    fn post_drain(&self, attachment: AttachmentId) -> Result<(), Self::Error> {
        self.mailbox
            .enqueue(attachment)
            .map(|_| ())
            .map_err(|error| {
                let MailboxEnqueueError::Full(attachment) = error;
                DrainPostQueueFull { attachment }
            })
    }
}

impl DrainPostDrain<'_> {
    /// Removes the next accepted attachment post.
    #[must_use]
    pub fn pop(&self) -> Option<AttachmentId> {
        self.drain.pop()
    }

    /// Completes this deterministic drain.
    ///
    /// [`DrainAfter::Pending`] means at least one accepted post remains for a
    /// later deterministic drain; it does not create a platform wake-up.
    #[must_use]
    pub fn finish(self) -> DrainAfter {
        self.drain.finish()
    }
}

impl fmt::Display for DrainPostQueueFull {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "drain post queue is full for attachment {}:{}",
            self.attachment.runtime_id().get(),
            self.attachment.epoch().get()
        )
    }
}

impl Error for DrainPostQueueFull {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelHost;
    use rustjsi_host::Host;

    #[test]
    fn fixed_capacity_returns_the_rejected_attachment() {
        let host = ModelHost::new().unwrap();
        let attachment = host.attachment_id();
        let queue = DrainPostQueue::new(NonZeroUsize::new(1).unwrap());

        queue.post_drain(attachment).unwrap();
        let error = queue
            .post_drain(attachment)
            .expect_err("fixed post capacity must reject a later attachment post");

        assert_eq!(error.attachment_id(), attachment);
        assert_eq!(queue.len(), 1);
        assert!(queue.is_drain_pending());
    }

    #[test]
    fn drain_preserves_fifo_posts_and_reports_a_successor() {
        let first = ModelHost::new().unwrap().attachment_id();
        let second = ModelHost::new().unwrap().attachment_id();
        let queue = DrainPostQueue::new(NonZeroUsize::new(2).unwrap());

        queue.post_drain(first).unwrap();
        queue.post_drain(second).unwrap();

        let DrainPostAcquire::Acquired(drain) = queue.acquire() else {
            panic!("accepted posts must acquire a drain");
        };
        assert_eq!(drain.pop(), Some(first));
        assert_eq!(drain.finish(), DrainAfter::Pending);

        let DrainPostAcquire::Acquired(successor) = queue.acquire() else {
            panic!("retained post must acquire a successor drain");
        };
        assert_eq!(successor.pop(), Some(second));
        assert_eq!(successor.finish(), DrainAfter::Idle);
        assert!(queue.is_empty());
    }
}
