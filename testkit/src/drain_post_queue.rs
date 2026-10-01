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
///
/// If an unfinished drain is dropped with retained attachment posts, the queue
/// preserves a pending recovery drain. A deterministic driver can inspect
/// [`DrainPostQueue::is_drain_pending`] after unwind and acquire that recovery
/// drain; no platform wake-up is created by this fixture.
///
/// ```compile_fail
/// use rustjsi_testkit::DrainPostDrain;
/// fn require_send<T: Send>() {}
/// require_send::<DrainPostDrain<'static>>();
/// ```
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
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn require_send_sync<T: Send + Sync>() {}

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

    #[test]
    fn dropped_drain_preserves_retained_posts_for_driver_recovery() {
        let first = ModelHost::new().unwrap().attachment_id();
        let second = ModelHost::new().unwrap().attachment_id();
        let queue = DrainPostQueue::new(NonZeroUsize::new(2).unwrap());

        queue.post_drain(first).unwrap();
        queue.post_drain(second).unwrap();

        let DrainPostAcquire::Acquired(drain) = queue.acquire() else {
            panic!("accepted posts must acquire an active drain");
        };
        assert_eq!(drain.pop(), Some(first));
        drop(drain);

        assert!(queue.is_drain_pending());
        let DrainPostAcquire::Acquired(recovery) = queue.acquire() else {
            panic!("dropped drain must preserve a recoverable successor");
        };
        assert_eq!(recovery.pop(), Some(second));
        assert_eq!(recovery.finish(), DrainAfter::Idle);
    }

    #[test]
    fn producer_threads_share_inert_posts_without_host_entry() {
        const PRODUCERS: usize = 8;

        require_send_sync::<DrainPostQueue>();

        let attachment = ModelHost::new().unwrap().attachment_id();
        let queue = Arc::new(DrainPostQueue::new(NonZeroUsize::new(PRODUCERS).unwrap()));
        let barrier = Arc::new(Barrier::new(PRODUCERS + 1));
        let mut workers = Vec::with_capacity(PRODUCERS);

        for _ in 0..PRODUCERS {
            let queue = Arc::clone(&queue);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                queue.post_drain(attachment)
            }));
        }

        barrier.wait();
        for worker in workers {
            worker
                .join()
                .expect("producer must not panic")
                .expect("fixed capacity must retain every producer post");
        }

        let DrainPostAcquire::Acquired(drain) = queue.acquire() else {
            panic!("accepted producer posts must acquire one drain");
        };
        let mut accepted = 0;
        while let Some(posted_attachment) = drain.pop() {
            assert_eq!(posted_attachment, attachment);
            accepted += 1;
        }
        assert_eq!(accepted, PRODUCERS);
        assert_eq!(drain.finish(), DrainAfter::Idle);
    }
}
