// SPDX-License-Identifier: MIT OR Apache-2.0

use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

use crossbeam_queue::ArrayQueue;

use crate::{
    DrainAcquire, DrainAfter, DrainPermit, DrainRequest, DrainSignal, DrainState, IngressClose,
    IngressGate, IngressPermit, IngressReserveError, IngressSealError, IngressState,
};

const NORMAL: u8 = 0;
const TERMINAL_PENDING: u8 = 1;
const TERMINAL_ACTIVE: u8 = 2;
const TERMINAL_CLOSED: u8 = 3;

/// Observable lifecycle state of a [`ClosableMailbox`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosableMailboxState {
    /// Producers and normal drains remain admissible.
    Open,
    /// Producer close has begun but terminal ownership has not started.
    Closing,
    /// Residual work awaits a terminal owner.
    TerminalPending,
    /// One terminal owner is transferring residual work.
    TerminalDraining,
    /// Terminal ownership completed with no residual work.
    Closed,
}

/// Failure to enqueue work into a [`ClosableMailbox`].
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub enum ClosableMailboxEnqueueError<T> {
    /// The fixed-capacity mailbox retained its existing work.
    Full(T),
    /// Producer ingress no longer accepts new work.
    Closed(T),
}

/// Failure while enqueueing work and accepting its required initial post.
#[derive(Debug)]
#[must_use]
pub enum ClosableMailboxPostError<T, E> {
    /// Queue admission rejected and returned the original payload.
    Enqueue(ClosableMailboxEnqueueError<T>),
    /// The payload is retained, but the requested host post failed.
    Post(E),
}

/// Result of attempting to acquire a normal drain.
#[derive(Debug)]
#[must_use]
pub enum ClosableMailboxAcquire<'mailbox, T> {
    /// No normal drain is pending.
    Idle,
    /// The runtime thread owns one normal drain.
    Acquired(ClosableMailboxDrain<'mailbox, T>),
    /// A different consumer owns the normal drain.
    Busy,
    /// Terminal ownership prevents normal drain acquisition.
    Unavailable(ClosableMailboxState),
}

/// Why a terminal drain cannot yet be acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum TerminalAcquireError {
    /// Producer close has not begun.
    ProducerCloseNotStarted,
    /// A producer admitted before close still owns a publication attempt.
    ProducerReservationsRemain(NonZeroUsize),
    /// A normal drain admission still owns the mailbox.
    NormalDrainReservationsRemain(NonZeroUsize),
    /// Another terminal owner is currently active.
    TerminalDrainActive,
    /// Terminal close already completed.
    Closed,
}

/// Failure to finish a [`TerminalMailboxDrain`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum TerminalDrainFinishError {
    /// Residual payloads must be transferred or released explicitly first.
    PayloadsRemain(NonZeroUsize),
    /// The terminal drain no longer owns the mailbox.
    Stale,
}

/// Fixed-capacity mailbox with explicit non-blocking terminal ownership.
///
/// This type is experimental. It has no scheduler, host-entry, attachment, or
/// engine policy. Its two admission gates prevent a terminal owner from racing
/// either a producer publication or normal-drain acquisition.
#[derive(Debug)]
pub struct ClosableMailbox<T> {
    queue: ArrayQueue<T>,
    signal: DrainSignal,
    producer_gate: IngressGate,
    normal_drain_gate: IngressGate,
    terminal_state: AtomicU8,
}

/// Affine ownership of one normal [`ClosableMailbox`] drain.
#[derive(Debug)]
#[must_use = "finish the drain or allow drop to restore normal drain state"]
pub struct ClosableMailboxDrain<'mailbox, T> {
    queue: &'mailbox ArrayQueue<T>,
    signal: &'mailbox DrainSignal,
    permit: DrainPermit<'mailbox>,
    _admission: IngressPermit<'mailbox>,
    finished: bool,
}

/// Affine ownership of residual work after normal draining has closed.
#[derive(Debug)]
#[must_use = "finish or release terminal ownership explicitly"]
pub struct TerminalMailboxDrain<'mailbox, T> {
    mailbox: &'mailbox ClosableMailbox<T>,
    finished: bool,
    _affine: PhantomData<Rc<()>>,
}

impl<T> ClosableMailbox<T> {
    /// Creates an open mailbox with fixed non-zero payload capacity.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            queue: ArrayQueue::new(capacity.get()),
            signal: DrainSignal::new(),
            producer_gate: IngressGate::new(),
            normal_drain_gate: IngressGate::new(),
            terminal_state: AtomicU8::new(NORMAL),
        }
    }

    /// Returns the fixed payload capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.queue.capacity()
    }

    /// Returns a concurrent payload-count snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Returns whether this instant contains no retained payload.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Returns whether one normal drain remains pending for host posting.
    ///
    /// This is a concurrent snapshot. A host uses it after a reported post
    /// failure to decide whether a retry remains useful.
    #[must_use]
    pub fn is_drain_pending(&self) -> bool {
        self.signal.state() == DrainState::Pending
    }

    /// Returns the current mailbox lifecycle snapshot.
    #[must_use]
    pub fn state(&self) -> ClosableMailboxState {
        match self.terminal_state.load(Ordering::Acquire) {
            TERMINAL_PENDING => ClosableMailboxState::TerminalPending,
            TERMINAL_ACTIVE => ClosableMailboxState::TerminalDraining,
            TERMINAL_CLOSED => ClosableMailboxState::Closed,
            NORMAL => match self.producer_gate.state() {
                IngressState::Open => ClosableMailboxState::Open,
                IngressState::Closing | IngressState::Sealed => ClosableMailboxState::Closing,
            },
            _ => unreachable!("ClosableMailbox contains only valid terminal states"),
        }
    }

    /// Enqueues one payload and reports whether a host post is required.
    ///
    /// # Errors
    ///
    /// Returns the original payload when fixed capacity is exhausted or producer
    /// admission has closed.
    pub fn enqueue(
        &self,
        value: T,
    ) -> Result<crate::MailboxEnqueue, ClosableMailboxEnqueueError<T>> {
        let admission = match self.producer_gate.reserve() {
            Ok(admission) => admission,
            Err(
                IngressReserveError::NotOpen(_) | IngressReserveError::ReservationSpaceExhausted,
            ) => return Err(ClosableMailboxEnqueueError::Closed(value)),
        };
        self.queue
            .push(value)
            .map_err(ClosableMailboxEnqueueError::Full)?;
        let outcome = match self.signal.request() {
            DrainRequest::Scheduled => crate::MailboxEnqueue::Scheduled,
            DrainRequest::Coalesced => crate::MailboxEnqueue::Coalesced,
            DrainRequest::Closed => unreachable!("producer admission prevents this race"),
        };
        drop(admission);
        Ok(outcome)
    }

    /// Enqueues one payload and accepts its initial host post before release.
    ///
    /// `post` runs only when this enqueue creates a new drain obligation. The
    /// producer admission remains live through that call, so terminal close
    /// cannot claim retained work between scheduling and post acceptance.
    ///
    /// # Errors
    ///
    /// Queue admission errors return the original payload. A post error leaves
    /// the payload retained with its pending drain obligation intact.
    pub fn enqueue_and_post<E>(
        &self,
        value: T,
        post: impl FnOnce() -> Result<(), E>,
    ) -> Result<crate::MailboxEnqueue, ClosableMailboxPostError<T, E>> {
        let admission = match self.producer_gate.reserve() {
            Ok(admission) => admission,
            Err(
                IngressReserveError::NotOpen(_) | IngressReserveError::ReservationSpaceExhausted,
            ) => {
                return Err(ClosableMailboxPostError::Enqueue(
                    ClosableMailboxEnqueueError::Closed(value),
                ));
            }
        };
        self.queue.push(value).map_err(|value| {
            ClosableMailboxPostError::Enqueue(ClosableMailboxEnqueueError::Full(value))
        })?;
        let outcome = match self.signal.request() {
            DrainRequest::Scheduled => crate::MailboxEnqueue::Scheduled,
            DrainRequest::Coalesced => crate::MailboxEnqueue::Coalesced,
            DrainRequest::Closed => unreachable!("producer admission prevents this race"),
        };
        if outcome == crate::MailboxEnqueue::Scheduled {
            post().map_err(ClosableMailboxPostError::Post)?;
        }
        drop(admission);
        Ok(outcome)
    }

    /// Stops admitting new producers without waiting for earlier ones.
    pub fn begin_close(&self) -> IngressClose {
        self.producer_gate.begin_close()
    }

    /// Acquires a normal runtime-thread drain while normal admission remains open.
    pub fn acquire(&self) -> ClosableMailboxAcquire<'_, T> {
        let Ok(admission) = self.normal_drain_gate.reserve() else {
            return ClosableMailboxAcquire::Unavailable(self.state());
        };
        match self.signal.acquire() {
            DrainAcquire::Idle => ClosableMailboxAcquire::Idle,
            DrainAcquire::Busy => ClosableMailboxAcquire::Busy,
            DrainAcquire::Closed => ClosableMailboxAcquire::Unavailable(self.state()),
            DrainAcquire::Acquired(permit) => {
                ClosableMailboxAcquire::Acquired(ClosableMailboxDrain {
                    queue: &self.queue,
                    signal: &self.signal,
                    permit,
                    _admission: admission,
                    finished: false,
                })
            }
        }
    }

    /// Attempts terminal ownership without waiting for producers or normal drains.
    ///
    /// # Errors
    ///
    /// Returns the outstanding producer or normal-drain admission count until
    /// terminal ownership is safe, or reports another active/closed terminal owner.
    pub fn try_begin_terminal_drain(
        &self,
    ) -> Result<TerminalMailboxDrain<'_, T>, TerminalAcquireError> {
        match self.producer_gate.try_seal() {
            Ok(IngressState::Sealed) => {}
            Ok(IngressState::Open | IngressState::Closing) => {
                unreachable!("seal only returns sealed")
            }
            Err(IngressSealError::StillOpen) => {
                return Err(TerminalAcquireError::ProducerCloseNotStarted);
            }
            Err(IngressSealError::ReservationsRemain(count)) => {
                return Err(TerminalAcquireError::ProducerReservationsRemain(count));
            }
        }
        let _ = self.normal_drain_gate.begin_close();
        match self.normal_drain_gate.try_seal() {
            Ok(IngressState::Sealed) => {}
            Ok(IngressState::Open | IngressState::Closing) => {
                unreachable!("seal only returns sealed")
            }
            Err(IngressSealError::StillOpen) => unreachable!("normal close began above"),
            Err(IngressSealError::ReservationsRemain(count)) => {
                return Err(TerminalAcquireError::NormalDrainReservationsRemain(count));
            }
        }
        let _ = self.signal.close();
        loop {
            match self.terminal_state.load(Ordering::Acquire) {
                NORMAL | TERMINAL_PENDING => {
                    let current = self.terminal_state.load(Ordering::Acquire);
                    if matches!(current, NORMAL | TERMINAL_PENDING)
                        && self
                            .terminal_state
                            .compare_exchange_weak(
                                current,
                                TERMINAL_ACTIVE,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                    {
                        return Ok(TerminalMailboxDrain {
                            mailbox: self,
                            finished: false,
                            _affine: PhantomData,
                        });
                    }
                }
                TERMINAL_ACTIVE => return Err(TerminalAcquireError::TerminalDrainActive),
                TERMINAL_CLOSED => return Err(TerminalAcquireError::Closed),
                _ => unreachable!("ClosableMailbox contains only valid terminal states"),
            }
        }
    }
}

impl<T> ClosableMailboxDrain<'_, T> {
    /// Removes the next normal payload.
    #[must_use]
    pub fn pop(&self) -> Option<T> {
        self.queue.pop()
    }

    /// Finishes normal draining and invokes `operation` before releasing admission.
    ///
    /// The drain signal has settled before `operation` runs, but terminal
    /// ownership remains blocked until it returns. This lets a host post a
    /// required successor without a close transition winning between finishing
    /// the normal drain and accepting that post.
    pub fn finish_with<R>(mut self, operation: impl FnOnce(DrainAfter) -> R) -> R {
        self.preserve_retained_payloads();
        let after = self.permit.finish_in_place();
        self.finished = true;
        operation(after)
    }

    /// Finishes normal draining and releases its admission.
    pub fn finish(self) -> DrainAfter {
        self.finish_with(|after| after)
    }

    fn preserve_retained_payloads(&self) {
        if !self.queue.is_empty() {
            match self.signal.request() {
                DrainRequest::Scheduled => {
                    unreachable!("an active drain cannot schedule independently")
                }
                DrainRequest::Coalesced => {}
                DrainRequest::Closed => unreachable!("terminal close waits for normal admission"),
            }
        }
    }
}

impl<T> Drop for ClosableMailboxDrain<'_, T> {
    fn drop(&mut self) {
        if !self.finished {
            self.preserve_retained_payloads();
        }
    }
}

impl<T> TerminalMailboxDrain<'_, T> {
    /// Removes one residual payload with terminal caller ownership.
    #[must_use]
    pub fn pop(&self) -> Option<T> {
        self.mailbox.queue.pop()
    }

    /// Completes terminal close once every residual payload has transferred.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalDrainFinishError::PayloadsRemain`] rather than dropping
    /// residual work, or [`TerminalDrainFinishError::Stale`] if this lease no
    /// longer owns the mailbox.
    pub fn finish(mut self) -> Result<(), TerminalDrainFinishError> {
        let Some(remaining) = NonZeroUsize::new(self.mailbox.queue.len()) else {
            if self
                .mailbox
                .terminal_state
                .compare_exchange(
                    TERMINAL_ACTIVE,
                    TERMINAL_CLOSED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                return Err(TerminalDrainFinishError::Stale);
            }
            self.finished = true;
            return Ok(());
        };
        Err(TerminalDrainFinishError::PayloadsRemain(remaining))
    }
}

impl<T> Drop for TerminalMailboxDrain<'_, T> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.mailbox.terminal_state.compare_exchange(
                TERMINAL_ACTIVE,
                TERMINAL_PENDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;
    use crate::{DrainAfter, MailboxEnqueue};

    fn capacity(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test capacity must be non-zero")
    }

    #[test]
    fn terminal_ownership_requires_producer_close() {
        let mailbox = ClosableMailbox::<()>::new(capacity(1));

        assert!(matches!(
            mailbox.try_begin_terminal_drain(),
            Err(TerminalAcquireError::ProducerCloseNotStarted)
        ));
        assert_eq!(mailbox.state(), ClosableMailboxState::Open);
    }

    #[test]
    fn terminal_drain_transfers_payloads_and_closes_late_ingress() {
        let mailbox = ClosableMailbox::new(capacity(2));
        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));
        assert_eq!(mailbox.enqueue(2), Ok(MailboxEnqueue::Coalesced));
        assert!(matches!(
            mailbox.begin_close(),
            IngressClose::Started { .. }
        ));

        let terminal = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(mailbox.state(), ClosableMailboxState::TerminalDraining);
        assert_eq!(terminal.pop(), Some(1));
        assert_eq!(terminal.pop(), Some(2));
        assert_eq!(terminal.pop(), None);
        terminal.finish().unwrap();

        assert_eq!(mailbox.state(), ClosableMailboxState::Closed);
        assert_eq!(
            mailbox.enqueue(3),
            Err(ClosableMailboxEnqueueError::Closed(3))
        );
        assert!(matches!(
            mailbox.acquire(),
            ClosableMailboxAcquire::Unavailable(ClosableMailboxState::Closed)
        ));
    }

    #[test]
    fn initial_post_runs_before_terminal_can_claim_producer_work() {
        let mailbox = ClosableMailbox::new(capacity(1));
        let outcome = mailbox.enqueue_and_post(7, || {
            let _ = mailbox.begin_close();
            assert!(matches!(
                mailbox.try_begin_terminal_drain(),
                Err(TerminalAcquireError::ProducerReservationsRemain(count)) if count.get() == 1
            ));
            Ok::<_, ()>(())
        });
        assert!(matches!(outcome, Ok(MailboxEnqueue::Scheduled)));

        let terminal = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(terminal.pop(), Some(7));
        terminal.finish().unwrap();
    }

    #[test]
    fn active_normal_drain_blocks_terminal_ownership_until_finish() {
        let mailbox = ClosableMailbox::new(capacity(2));
        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));
        let ClosableMailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("scheduled work must acquire a normal drain");
        };
        assert_eq!(drain.pop(), Some(1));
        let _ = mailbox.begin_close();

        assert!(matches!(
            mailbox.try_begin_terminal_drain(),
            Err(TerminalAcquireError::NormalDrainReservationsRemain(
                count
            ))
            if count.get() == 1
        ));
        assert_eq!(drain.finish(), DrainAfter::Idle);

        let terminal = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(terminal.pop(), None);
        terminal.finish().unwrap();
        assert_eq!(mailbox.state(), ClosableMailboxState::Closed);
    }

    #[test]
    fn finish_callback_runs_before_terminal_admission_releases() {
        let mailbox = ClosableMailbox::new(capacity(1));
        assert_eq!(mailbox.enqueue(7), Ok(MailboxEnqueue::Scheduled));
        let ClosableMailboxAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("scheduled work must acquire a normal drain");
        };
        assert_eq!(drain.pop(), Some(7));
        let _ = mailbox.begin_close();

        let terminal_attempt = drain.finish_with(|after| {
            assert_eq!(after, DrainAfter::Idle);
            mailbox.try_begin_terminal_drain()
        });
        assert!(matches!(
            terminal_attempt,
            Err(TerminalAcquireError::NormalDrainReservationsRemain(count)) if count.get() == 1
        ));

        let terminal = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(terminal.pop(), None);
        terminal.finish().unwrap();
    }

    #[test]
    fn dropped_terminal_owner_preserves_residual_work_without_reopening_normal_drains() {
        let mailbox = ClosableMailbox::new(capacity(2));
        assert_eq!(mailbox.enqueue(1), Ok(MailboxEnqueue::Scheduled));
        assert_eq!(mailbox.enqueue(2), Ok(MailboxEnqueue::Coalesced));
        let _ = mailbox.begin_close();

        let terminal = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(terminal.pop(), Some(1));
        assert_eq!(
            terminal.finish(),
            Err(TerminalDrainFinishError::PayloadsRemain(
                NonZeroUsize::new(1).expect("one payload remains")
            ))
        );
        assert_eq!(mailbox.state(), ClosableMailboxState::TerminalPending);
        assert!(matches!(
            mailbox.acquire(),
            ClosableMailboxAcquire::Unavailable(ClosableMailboxState::TerminalPending)
        ));

        let resumed = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(resumed.pop(), Some(2));
        resumed.finish().unwrap();
    }

    #[test]
    fn concurrent_terminal_claims_grant_one_owner_and_preserve_retryable_work() {
        let mailbox = Arc::new(ClosableMailbox::new(capacity(1)));
        assert_eq!(mailbox.enqueue(7), Ok(MailboxEnqueue::Scheduled));
        let _ = mailbox.begin_close();

        let start = Arc::new(Barrier::new(3));
        let release_owner = Arc::new(Barrier::new(2));
        let (sender, receiver) = mpsc::channel();
        let mut workers = Vec::new();

        for _ in 0..2 {
            let mailbox = Arc::clone(&mailbox);
            let start = Arc::clone(&start);
            let release_owner = Arc::clone(&release_owner);
            let sender = sender.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                let result = mailbox.try_begin_terminal_drain();
                let granted = result.is_ok();
                sender.send(granted).expect("coordinator must be available");
                if let Ok(drain) = result {
                    release_owner.wait();
                    drop(drain);
                }
            }));
        }
        drop(sender);

        start.wait();
        let grants = receiver.iter().take(2).filter(|granted| *granted).count();
        assert_eq!(grants, 1);
        release_owner.wait();
        for worker in workers {
            worker.join().expect("terminal claimant must not panic");
        }

        assert_eq!(mailbox.state(), ClosableMailboxState::TerminalPending);
        let retry = mailbox.try_begin_terminal_drain().unwrap();
        assert_eq!(retry.pop(), Some(7));
        retry.finish().unwrap();
    }

    #[test]
    fn producer_publication_keeps_normal_signal_open_during_terminal_close() {
        for _ in 0..128 {
            let mailbox = Arc::new(ClosableMailbox::new(capacity(1)));
            let start = Arc::new(Barrier::new(2));

            let producer_mailbox = Arc::clone(&mailbox);
            let producer_start = Arc::clone(&start);
            let producer = thread::spawn(move || {
                producer_start.wait();
                producer_mailbox.enqueue(7)
            });

            start.wait();
            let _ = mailbox.begin_close();
            let terminal = loop {
                match mailbox.try_begin_terminal_drain() {
                    Ok(drain) => break drain,
                    Err(TerminalAcquireError::ProducerReservationsRemain(_)) => {
                        thread::yield_now();
                    }
                    Err(error) => {
                        panic!("producer-close race must remain terminally drainable: {error:?}")
                    }
                }
            };

            match producer.join().expect("producer must not panic") {
                Ok(MailboxEnqueue::Scheduled) => assert_eq!(terminal.pop(), Some(7)),
                Ok(MailboxEnqueue::Coalesced) => {
                    panic!("empty mailbox cannot coalesce its first publication")
                }
                Err(ClosableMailboxEnqueueError::Closed(7)) => assert_eq!(terminal.pop(), None),
                Err(ClosableMailboxEnqueueError::Full(value)) => {
                    panic!("empty mailbox cannot be full: {value}")
                }
                Err(ClosableMailboxEnqueueError::Closed(value)) => {
                    panic!("producer payload must be preserved: {value}")
                }
            }
            assert_eq!(terminal.pop(), None);
            terminal.finish().unwrap();
        }
    }
}
