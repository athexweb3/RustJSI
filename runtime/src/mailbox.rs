// SPDX-License-Identifier: MIT OR Apache-2.0

use std::num::NonZeroUsize;

use crossbeam_queue::ArrayQueue;

use crate::{DrainAcquire, DrainAfter, DrainPermit, DrainRequest, DrainSignal, DrainState};

/// Result of placing work into a [`BoundedMailbox`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum MailboxEnqueue {
    /// The host must post one future runtime-thread drain.
    Scheduled,
    /// A pending or active drain already represents this work.
    Coalesced,
}

/// Failure to place work into a [`BoundedMailbox`].
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub enum MailboxEnqueueError<T> {
    /// The fixed-capacity mailbox was full; the original work is returned.
    Full(T),
}

/// Result of trying to begin a [`BoundedMailbox`] drain.
#[derive(Debug)]
#[must_use]
pub enum MailboxAcquire<'mailbox, T> {
    /// No drain is pending.
    Idle,
    /// The runtime thread owns the current drain.
    Acquired(MailboxDrain<'mailbox, T>),
    /// A different consumer owns the current drain.
    Busy,
}

/// Fixed-capacity payload mailbox paired with a coalesced drain signal.
///
/// The mailbox has a fixed allocation at construction. Producers retain an
/// explicit ownership path when it is full: [`MailboxEnqueueError::Full`]
/// returns their original value. A successful enqueue reports whether the host
/// must post one future runtime-thread drain; this type does not perform that
/// post itself.
///
/// `BoundedMailbox` deliberately does not expose close or cancellation. Those
/// operations need a runtime-owner protocol that defines terminal ownership of
/// payloads concurrently with producers and are separate from this queue.
#[derive(Debug)]
pub struct BoundedMailbox<T> {
    queue: ArrayQueue<T>,
    signal: DrainSignal,
}

/// Affine ownership of one active [`BoundedMailbox`] drain.
///
/// The host must acquire this permit only while it holds the applicable
/// runtime-thread authority. If it is dropped with retained payloads, the
/// mailbox preserves a pending successor drain. A host panic boundary can use
/// [`BoundedMailbox::is_drain_pending`] to decide whether it must post that
/// successor after unwinding.
///
/// ```compile_fail
/// use rustjsi_runtime::MailboxDrain;
/// fn require_send<T: Send>() {}
/// require_send::<MailboxDrain<'static, ()>>();
/// ```
///
/// ```compile_fail
/// use rustjsi_runtime::MailboxDrain;
/// fn require_sync<T: Sync>() {}
/// require_sync::<MailboxDrain<'static, ()>>();
/// ```
#[derive(Debug)]
#[must_use = "finish the drain or handle host recovery after it is dropped"]
pub struct MailboxDrain<'mailbox, T> {
    queue: &'mailbox ArrayQueue<T>,
    signal: &'mailbox DrainSignal,
    permit: DrainPermit<'mailbox>,
    finished: bool,
}

impl<T> BoundedMailbox<T> {
    /// Creates a mailbox with a fixed non-zero payload capacity.
    ///
    /// This allocates the queue's complete payload capacity now. Construction
    /// may panic if the requested capacity is too large for the queue
    /// implementation or the allocation cannot be satisfied.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            queue: ArrayQueue::new(capacity.get()),
            signal: DrainSignal::new(),
        }
    }

    /// Returns the maximum number of payloads retained at once.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.queue.capacity()
    }

    /// Returns the number of payloads retained at this instant.
    ///
    /// The returned value is a concurrent snapshot and can be stale before it
    /// reaches the caller.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Returns whether this instant's snapshot contains no payloads.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Returns whether a future runtime-thread drain is currently pending.
    ///
    /// This is a concurrent snapshot. It is intended for a host's panic or
    /// unwind recovery boundary after a [`MailboxDrain`] has been dropped; on
    /// the normal path, use [`MailboxDrain::finish`] and act on its
    /// [`DrainAfter`] result.
    #[must_use]
    pub fn is_drain_pending(&self) -> bool {
        self.signal.state() == DrainState::Pending
    }

    /// Enqueues one owned payload and reports the corresponding drain action.
    ///
    /// On a full mailbox, the original payload is returned and no scheduling
    /// state changes. On success, [`MailboxEnqueue::Scheduled`] means the
    /// caller must arrange a future runtime-thread drain.
    ///
    /// # Errors
    ///
    /// Returns [`MailboxEnqueueError::Full`] with the original payload when no
    /// fixed-capacity slot is available.
    pub fn enqueue(&self, value: T) -> Result<MailboxEnqueue, MailboxEnqueueError<T>> {
        self.queue.push(value).map_err(MailboxEnqueueError::Full)?;

        match self.signal.request() {
            DrainRequest::Scheduled => Ok(MailboxEnqueue::Scheduled),
            DrainRequest::Coalesced => Ok(MailboxEnqueue::Coalesced),
            DrainRequest::Closed => unreachable!("mailbox does not expose signal closure"),
        }
    }

    /// Acquires a pending drain for the runtime thread.
    pub fn acquire(&self) -> MailboxAcquire<'_, T> {
        match self.signal.acquire() {
            DrainAcquire::Idle => MailboxAcquire::Idle,
            DrainAcquire::Acquired(permit) => MailboxAcquire::Acquired(MailboxDrain {
                queue: &self.queue,
                signal: &self.signal,
                permit,
                finished: false,
            }),
            DrainAcquire::Busy => MailboxAcquire::Busy,
            DrainAcquire::Closed => unreachable!("mailbox does not expose signal closure"),
        }
    }
}

impl<T> MailboxDrain<'_, T> {
    /// Removes the next retained payload, if one remains.
    #[must_use]
    pub fn pop(&self) -> Option<T> {
        self.queue.pop()
    }

    /// Completes this drain and reports whether the host must post a successor.
    ///
    /// Finishing with retained payloads preserves a pending successor drain so
    /// the host cannot accidentally strand those payloads by ending early.
    pub fn finish(mut self) -> DrainAfter {
        self.preserve_retained_payloads();
        let after = self.permit.finish_in_place();
        self.finished = true;
        after
    }

    fn preserve_retained_payloads(&self) {
        if !self.queue.is_empty() {
            match self.signal.request() {
                DrainRequest::Coalesced => {}
                DrainRequest::Scheduled => {
                    unreachable!("an active mailbox drain cannot schedule independently")
                }
                DrainRequest::Closed => unreachable!("mailbox does not expose signal closure"),
            }
        }
    }
}

impl<T> Drop for MailboxDrain<'_, T> {
    fn drop(&mut self) {
        if !self.finished {
            self.preserve_retained_payloads();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::num::NonZeroUsize;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    fn capacity(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test capacity must be non-zero")
    }

    #[test]
    fn fixed_capacity_rejects_and_returns_the_original_payload() {
        let mailbox = BoundedMailbox::new(capacity(1));
        assert_eq!(mailbox.capacity(), 1);
        assert_eq!(
            mailbox.enqueue(String::from("kept")),
            Ok(MailboxEnqueue::Scheduled)
        );

        let rejected = mailbox
            .enqueue(String::from("returned"))
            .expect_err("a full mailbox must reject the producer payload");
        assert_eq!(
            rejected,
            MailboxEnqueueError::Full(String::from("returned"))
        );
        assert_eq!(mailbox.len(), 1);

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("first enqueue must make a drain available");
        };
        assert_eq!(drain.pop(), Some(String::from("kept")));
        assert_eq!(drain.pop(), None);
        assert_eq!(drain.finish(), DrainAfter::Idle);
    }

    #[test]
    fn successful_enqueues_coalesce_one_host_post() {
        let mailbox = BoundedMailbox::new(capacity(2));

        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));
        assert_eq!(mailbox.enqueue(2), Ok(MailboxEnqueue::Coalesced));

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("pending mailbox must provide a drain");
        };
        assert_eq!(drain.pop(), Some(1));
        assert_eq!(drain.pop(), Some(2));
        assert_eq!(drain.finish(), DrainAfter::Idle);
        assert!(mailbox.is_empty());
        assert!(matches!(mailbox.acquire(), MailboxAcquire::Idle));
    }

    #[test]
    fn enqueue_during_a_drain_leaves_a_successor_pending() {
        let mailbox = BoundedMailbox::new(capacity(2));
        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("pending mailbox must provide a drain");
        };
        assert_eq!(drain.pop(), Some(1));
        assert_eq!(mailbox.enqueue(2), Ok(MailboxEnqueue::Coalesced));
        assert_eq!(drain.finish(), DrainAfter::Pending);

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("enqueue during drain must preserve a successor drain");
        };
        assert_eq!(drain.pop(), Some(2));
        assert_eq!(drain.finish(), DrainAfter::Idle);
    }

    #[test]
    fn early_finish_preserves_retained_payloads_for_a_successor() {
        let mailbox = BoundedMailbox::new(capacity(2));
        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));
        assert_eq!(mailbox.enqueue(2), Ok(MailboxEnqueue::Coalesced));

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("pending mailbox must provide a drain");
        };
        assert_eq!(drain.pop(), Some(1));
        assert_eq!(drain.finish(), DrainAfter::Pending);

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("retained payload must receive a successor drain");
        };
        assert_eq!(drain.pop(), Some(2));
        assert_eq!(drain.finish(), DrainAfter::Idle);
    }

    #[test]
    fn dropped_drain_preserves_retained_payloads_for_host_recovery() {
        let mailbox = BoundedMailbox::new(capacity(2));
        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));
        assert_eq!(mailbox.enqueue(2), Ok(MailboxEnqueue::Coalesced));

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("pending mailbox must provide a drain");
        };
        assert_eq!(drain.pop(), Some(1));
        drop(drain);

        assert!(mailbox.is_drain_pending());
        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("recovery must observe the retained payload's pending drain");
        };
        assert_eq!(drain.pop(), Some(2));
        assert_eq!(drain.finish(), DrainAfter::Idle);
    }

    #[test]
    fn concurrent_producers_preserve_each_payload_and_one_post_obligation() {
        const PRODUCERS: usize = 12;

        let mailbox = Arc::new(BoundedMailbox::new(capacity(PRODUCERS)));
        let barrier = Arc::new(Barrier::new(PRODUCERS));
        let mut workers = Vec::with_capacity(PRODUCERS);

        for value in 0..PRODUCERS {
            let mailbox = Arc::clone(&mailbox);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                mailbox
                    .enqueue(value)
                    .expect("capacity accommodates every producer")
            }));
        }

        let mut scheduled = 0;
        for worker in workers {
            if worker.join().expect("producer must not panic") == MailboxEnqueue::Scheduled {
                scheduled += 1;
            }
        }
        assert_eq!(scheduled, 1);

        let MailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("producer burst must leave one drain pending");
        };
        let mut values = BTreeSet::new();
        while let Some(value) = drain.pop() {
            assert!(values.insert(value), "each payload must appear once");
        }
        assert_eq!(values.len(), PRODUCERS);
        assert_eq!(drain.finish(), DrainAfter::Idle);
    }
}
