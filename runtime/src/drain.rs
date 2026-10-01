// SPDX-License-Identifier: MIT OR Apache-2.0

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const DRAINING: u8 = 2;
const DRAINING_PENDING: u8 = 3;
const CLOSED: u8 = 4;

/// Observable state of a coalesced drain request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainState {
    /// No drain is pending or executing.
    Idle,
    /// A host must schedule one drain.
    Pending,
    /// A runtime-thread consumer owns the current drain.
    Draining,
    /// A consumer owns the current drain and another drain is required after it.
    DrainingWithPending,
    /// The signal is terminal and rejects later requests.
    Closed,
}

/// Result of asking a [`DrainSignal`] to schedule a drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum DrainRequest {
    /// The caller must arrange one host-specific future drain.
    Scheduled,
    /// An existing pending or active drain already represents this request.
    Coalesced,
    /// The owning runtime has closed and cannot accept new work.
    Closed,
}

/// Result of attempting to acquire the current drain.
#[derive(Debug)]
#[must_use]
pub enum DrainAcquire<'signal> {
    /// No work is pending.
    Idle,
    /// The caller owns the current runtime-thread drain.
    Acquired(DrainPermit<'signal>),
    /// Another consumer already owns the current drain.
    Busy,
    /// The owning runtime has closed.
    Closed,
}

/// State observed after a drain permit completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum DrainAfter {
    /// No later drain is required.
    Idle,
    /// Another request arrived while the permit was live.
    Pending,
    /// The signal was closed while the permit was live.
    Closed,
}

/// Result of closing a [`DrainSignal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum DrainClose {
    /// Closing won the transition from the reported prior state.
    Closed(DrainState),
    /// The signal was already closed.
    AlreadyClosed,
}

/// Allocation-free coalescing state for one future runtime-thread drain.
///
/// The signal stores no work payload and does not grant runtime entry. It only
/// records whether a host needs to arrange one drain. Producers may request a
/// drain from any permitted thread; the host keeps scheduling policy and the
/// resources represented by the drain outside this type.
#[derive(Debug)]
pub struct DrainSignal {
    state: AtomicU8,
}

/// Affine ownership of one active [`DrainSignal`] drain.
///
/// Dropping an unfinished permit restores a non-draining state. If a request
/// arrived while the permit was live, the signal becomes pending; the host's
/// unwind boundary must inspect that state and post the successor drain. The
/// host must keep runtime-thread authority for the full permit lifetime.
///
/// ```compile_fail
/// use rustjsi_runtime::DrainPermit;
/// fn require_send<T: Send>() {}
/// require_send::<DrainPermit<'static>>();
/// ```
///
/// ```compile_fail
/// use rustjsi_runtime::DrainPermit;
/// fn require_sync<T: Sync>() {}
/// require_sync::<DrainPermit<'static>>();
/// ```
#[derive(Debug)]
#[must_use = "finish the permit or allow its drop to restore the drain signal"]
pub struct DrainPermit<'signal> {
    signal: &'signal DrainSignal,
    finished: bool,
    _affine: PhantomData<Rc<()>>,
}

impl DrainSignal {
    /// Creates an idle signal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
        }
    }

    /// Returns the signal's current observable state.
    #[must_use]
    pub fn state(&self) -> DrainState {
        state_from_raw(self.state.load(Ordering::Acquire))
    }

    /// Requests a later drain without heap allocation or waiting for a consumer.
    ///
    /// [`DrainRequest::Scheduled`] means the caller must arrange one
    /// host-specific post. [`DrainRequest::Coalesced`] means a pending or active
    /// drain will preserve the request. A closed signal never reopens.
    pub fn request(&self) -> DrainRequest {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match current {
                IDLE => {
                    if self
                        .state
                        .compare_exchange_weak(IDLE, PENDING, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return DrainRequest::Scheduled;
                    }
                }
                PENDING | DRAINING_PENDING => return DrainRequest::Coalesced,
                DRAINING => {
                    if self
                        .state
                        .compare_exchange_weak(
                            DRAINING,
                            DRAINING_PENDING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return DrainRequest::Coalesced;
                    }
                }
                CLOSED => return DrainRequest::Closed,
                _ => unreachable!("DrainSignal contains only valid states"),
            }
        }
    }

    /// Acquires the currently pending drain for the runtime thread.
    pub fn acquire(&self) -> DrainAcquire<'_> {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match current {
                IDLE => return DrainAcquire::Idle,
                PENDING => {
                    if self
                        .state
                        .compare_exchange_weak(
                            PENDING,
                            DRAINING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return DrainAcquire::Acquired(DrainPermit {
                            signal: self,
                            finished: false,
                            _affine: PhantomData,
                        });
                    }
                }
                DRAINING | DRAINING_PENDING => return DrainAcquire::Busy,
                CLOSED => return DrainAcquire::Closed,
                _ => unreachable!("DrainSignal contains only valid states"),
            }
        }
    }

    /// Closes the signal and rejects future requests.
    ///
    /// Closing does not revoke an already-held host entry. A live permit observes
    /// [`DrainAfter::Closed`] when it finishes or unwinds.
    pub fn close(&self) -> DrainClose {
        loop {
            let current = self.state.load(Ordering::Acquire);
            if current == CLOSED {
                return DrainClose::AlreadyClosed;
            }
            if self
                .state
                .compare_exchange_weak(current, CLOSED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return DrainClose::Closed(state_from_raw(current));
            }
        }
    }

    fn finish_drain(&self) -> DrainAfter {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match current {
                DRAINING => {
                    if self
                        .state
                        .compare_exchange_weak(DRAINING, IDLE, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return DrainAfter::Idle;
                    }
                }
                DRAINING_PENDING => {
                    if self
                        .state
                        .compare_exchange_weak(
                            DRAINING_PENDING,
                            PENDING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return DrainAfter::Pending;
                    }
                }
                CLOSED | IDLE | PENDING => return DrainAfter::Closed,
                _ => unreachable!("DrainSignal contains only valid states"),
            }
        }
    }
}

impl Default for DrainSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl DrainPermit<'_> {
    /// Completes this drain and reports whether another request arrived.
    pub fn finish(mut self) -> DrainAfter {
        self.finish_in_place()
    }

    pub(crate) fn finish_in_place(&mut self) -> DrainAfter {
        let after = self.signal.finish_drain();
        self.finished = true;
        after
    }
}

impl Drop for DrainPermit<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.signal.finish_drain();
        }
    }
}

fn state_from_raw(value: u8) -> DrainState {
    match value {
        IDLE => DrainState::Idle,
        PENDING => DrainState::Pending,
        DRAINING => DrainState::Draining,
        DRAINING_PENDING => DrainState::DrainingWithPending,
        CLOSED => DrainState::Closed,
        _ => unreachable!("DrainSignal contains only valid states"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn requests_coalesce_until_one_consumer_finishes() {
        let signal = DrainSignal::new();

        assert_eq!(signal.request(), DrainRequest::Scheduled);
        assert_eq!(signal.request(), DrainRequest::Coalesced);
        assert_eq!(signal.state(), DrainState::Pending);

        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("pending signal must provide a permit");
        };
        assert_eq!(signal.state(), DrainState::Draining);
        assert_eq!(permit.finish(), DrainAfter::Idle);
        assert_eq!(signal.state(), DrainState::Idle);
    }

    #[test]
    fn request_during_drain_preserves_one_successor() {
        let signal = DrainSignal::new();
        assert_eq!(signal.request(), DrainRequest::Scheduled);
        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("pending signal must provide a permit");
        };

        assert_eq!(signal.request(), DrainRequest::Coalesced);
        assert_eq!(signal.state(), DrainState::DrainingWithPending);
        assert_eq!(permit.finish(), DrainAfter::Pending);
        assert_eq!(signal.state(), DrainState::Pending);

        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("successor request must provide a permit");
        };
        assert_eq!(permit.finish(), DrainAfter::Idle);
    }

    #[test]
    fn dropped_permit_cannot_strand_the_signal() {
        let signal = DrainSignal::new();
        assert_eq!(signal.request(), DrainRequest::Scheduled);
        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("pending signal must provide a permit");
        };

        drop(permit);
        assert_eq!(signal.state(), DrainState::Idle);
        assert_eq!(signal.request(), DrainRequest::Scheduled);
    }

    #[test]
    fn dropped_permit_preserves_a_request_that_arrived_during_drain() {
        let signal = DrainSignal::new();
        assert_eq!(signal.request(), DrainRequest::Scheduled);
        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("pending signal must provide a permit");
        };
        assert_eq!(signal.request(), DrainRequest::Coalesced);

        drop(permit);
        assert_eq!(signal.state(), DrainState::Pending);
    }

    #[test]
    fn close_is_terminal_for_requests_and_permits() {
        let signal = DrainSignal::new();
        assert_eq!(signal.request(), DrainRequest::Scheduled);
        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("pending signal must provide a permit");
        };

        assert_eq!(signal.close(), DrainClose::Closed(DrainState::Draining));
        assert_eq!(signal.request(), DrainRequest::Closed);
        assert_eq!(permit.finish(), DrainAfter::Closed);
        assert_eq!(signal.close(), DrainClose::AlreadyClosed);
        assert!(matches!(signal.acquire(), DrainAcquire::Closed));
    }

    #[test]
    fn concurrent_producers_schedule_once() {
        const PRODUCERS: usize = 16;

        let signal = Arc::new(DrainSignal::new());
        let barrier = Arc::new(Barrier::new(PRODUCERS));
        let handles = (0..PRODUCERS)
            .map(|_| {
                let signal = Arc::clone(&signal);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    signal.request()
                })
            })
            .collect::<Vec<_>>();

        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("producer must not panic"))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == DrainRequest::Scheduled)
                .count(),
            1
        );
        assert!(
            outcomes.iter().all(|outcome| matches!(
                outcome,
                DrainRequest::Scheduled | DrainRequest::Coalesced
            ))
        );

        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("coalesced producers must leave one pending drain");
        };
        assert_eq!(permit.finish(), DrainAfter::Idle);
    }

    #[test]
    fn concurrent_requests_during_drain_preserve_one_successor() {
        const PRODUCERS: usize = 16;

        let signal = Arc::new(DrainSignal::new());
        assert_eq!(signal.request(), DrainRequest::Scheduled);
        let DrainAcquire::Acquired(permit) = signal.acquire() else {
            panic!("pending signal must provide a permit");
        };

        let barrier = Arc::new(Barrier::new(PRODUCERS));
        let handles = (0..PRODUCERS)
            .map(|_| {
                let signal = Arc::clone(&signal);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    signal.request()
                })
            })
            .collect::<Vec<_>>();

        for outcome in handles
            .into_iter()
            .map(|handle| handle.join().expect("producer must not panic"))
        {
            assert_eq!(outcome, DrainRequest::Coalesced);
        }
        assert_eq!(signal.state(), DrainState::DrainingWithPending);
        assert_eq!(permit.finish(), DrainAfter::Pending);
        assert_eq!(signal.state(), DrainState::Pending);
    }
}
