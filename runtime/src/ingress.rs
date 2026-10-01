// SPDX-License-Identifier: MIT OR Apache-2.0

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

const PHASE_BITS: u32 = 2;
const PHASE_SHIFT: u32 = usize::BITS - PHASE_BITS;
const RESERVATION_MASK: usize = (1usize << PHASE_SHIFT) - 1;
const OPEN: usize = 0;
const CLOSING: usize = 1usize << PHASE_SHIFT;
const SEALED: usize = 2usize << PHASE_SHIFT;

/// Observable admission state of an [`IngressGate`].
///
/// This describes producer admission only. It does not describe the lifecycle
/// of queued payloads, an active runtime drain, or terminal resource ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressState {
    /// New producer reservations may be granted.
    Open,
    /// New reservations are rejected while earlier reservations settle.
    Closing,
    /// Earlier reservations have settled and no later reservation can exist.
    Sealed,
}

/// Failure to reserve producer ingress through an [`IngressGate`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum IngressReserveError {
    /// The gate no longer admits new producer reservations.
    NotOpen(IngressState),
    /// The bounded reservation counter cannot issue another admission safely.
    ReservationSpaceExhausted,
}

/// Result of beginning ingress closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum IngressClose {
    /// This call stopped new reservations while reporting earlier admissions.
    Started {
        /// Reservations granted before this close transition won.
        in_flight_reservations: usize,
    },
    /// Another caller already stopped new reservations.
    AlreadyClosing {
        /// Reservations granted before closure began that have not settled.
        in_flight_reservations: usize,
    },
    /// Earlier admissions have already settled and the gate is sealed.
    AlreadySealed,
}

/// Failure to seal an [`IngressGate`] after closure begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum IngressSealError {
    /// Closure has not begun.
    StillOpen,
    /// An earlier producer reservation must settle before sealing.
    ReservationsRemain(NonZeroUsize),
}

/// A non-blocking producer-admission gate for a future bounded work queue.
///
/// A producer reserves ingress before it touches payload storage. Once
/// [`Self::begin_close`] observes `Open -> Closing`, no later reservation can
/// succeed; reservations granted earlier remain explicitly counted until their
/// [`IngressPermit`] is settled or dropped. A close owner can therefore wait
/// without blocking by retrying [`Self::try_seal`].
///
/// This primitive intentionally owns no payload storage or scheduler action.
/// In particular, [`IngressState::Sealed`] proves only that no producer can
/// publish later work through this gate. A queue integration must separately
/// define terminal payload ownership and active-drain handling.
#[derive(Debug)]
pub struct IngressGate {
    state: AtomicUsize,
}

/// One earlier producer admission from an [`IngressGate`].
///
/// Dropping an unsettled permit releases its admission. The permit grants no
/// runtime, engine, callback, or queue authority; it only keeps the matching
/// ingress gate from sealing too early.
#[derive(Debug)]
#[must_use = "settle or drop the permit so a close owner can seal ingress"]
pub struct IngressPermit<'gate> {
    gate: &'gate IngressGate,
    settled: bool,
}

impl IngressGate {
    /// Creates an open ingress gate with no producer reservations.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(OPEN),
        }
    }

    /// Returns this instant's admission state.
    #[must_use]
    pub fn state(&self) -> IngressState {
        state_from_raw(self.state.load(Ordering::Acquire))
    }

    /// Returns this instant's count of earlier reservations not yet settled.
    ///
    /// The returned count is a concurrent snapshot and can change before it
    /// reaches the caller.
    #[must_use]
    pub fn in_flight_reservations(&self) -> usize {
        reservation_count(self.state.load(Ordering::Acquire))
    }

    /// Reserves one producer admission while ingress remains open.
    ///
    /// This operation does not wait for a close owner or consumer. A granted
    /// permit must be settled after the producer has either published its work
    /// or recovered ownership of a rejected payload.
    ///
    /// # Errors
    ///
    /// Returns [`IngressReserveError::NotOpen`] once closure starts, or
    /// [`IngressReserveError::ReservationSpaceExhausted`] instead of wrapping
    /// the in-flight reservation count.
    pub fn reserve(&self) -> Result<IngressPermit<'_>, IngressReserveError> {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match state_from_raw(current) {
                IngressState::Open => {
                    let count = reservation_count(current);
                    if count == RESERVATION_MASK {
                        return Err(IngressReserveError::ReservationSpaceExhausted);
                    }
                    if self
                        .state
                        .compare_exchange_weak(
                            current,
                            current + 1,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(IngressPermit {
                            gate: self,
                            settled: false,
                        });
                    }
                }
                state => return Err(IngressReserveError::NotOpen(state)),
            }
        }
    }

    /// Stops admitting new producer reservations.
    ///
    /// The transition preserves the count of reservations granted before this
    /// method won. It never waits for those producers to settle.
    pub fn begin_close(&self) -> IngressClose {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match state_from_raw(current) {
                IngressState::Open => {
                    let closing = CLOSING | reservation_count(current);
                    if self
                        .state
                        .compare_exchange_weak(
                            current,
                            closing,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return IngressClose::Started {
                            in_flight_reservations: reservation_count(current),
                        };
                    }
                }
                IngressState::Closing => {
                    return IngressClose::AlreadyClosing {
                        in_flight_reservations: reservation_count(current),
                    };
                }
                IngressState::Sealed => return IngressClose::AlreadySealed,
            }
        }
    }

    /// Seals ingress once every reservation granted before closure has settled.
    ///
    /// This method does not block. A close owner that receives
    /// [`IngressSealError::ReservationsRemain`] must arrange its own retry or
    /// completion notification; the gate deliberately has no scheduler policy.
    ///
    /// # Errors
    ///
    /// Returns [`IngressSealError::StillOpen`] before [`Self::begin_close`], or
    /// [`IngressSealError::ReservationsRemain`] while an earlier producer still
    /// holds an admission.
    pub fn try_seal(&self) -> Result<IngressState, IngressSealError> {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match state_from_raw(current) {
                IngressState::Open => return Err(IngressSealError::StillOpen),
                IngressState::Closing => {
                    let count = reservation_count(current);
                    let Some(remaining) = NonZeroUsize::new(count) else {
                        if self
                            .state
                            .compare_exchange_weak(
                                current,
                                SEALED,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            return Ok(IngressState::Sealed);
                        }
                        continue;
                    };
                    return Err(IngressSealError::ReservationsRemain(remaining));
                }
                IngressState::Sealed => return Ok(IngressState::Sealed),
            }
        }
    }

    fn settle_reservation(&self) {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            reservation_count(previous) > 0,
            "an ingress permit settles exactly one outstanding reservation"
        );
    }
}

impl Default for IngressGate {
    fn default() -> Self {
        Self::new()
    }
}

impl IngressPermit<'_> {
    /// Settles this producer admission without waiting for a close owner.
    pub fn settle(mut self) {
        self.settle_in_place();
    }

    fn settle_in_place(&mut self) {
        if !self.settled {
            self.gate.settle_reservation();
            self.settled = true;
        }
    }
}

impl Drop for IngressPermit<'_> {
    fn drop(&mut self) {
        self.settle_in_place();
    }
}

fn reservation_count(value: usize) -> usize {
    value & RESERVATION_MASK
}

fn state_from_raw(value: usize) -> IngressState {
    match value >> PHASE_SHIFT {
        0 => IngressState::Open,
        1 => IngressState::Closing,
        2 => IngressState::Sealed,
        _ => unreachable!("IngressGate contains only valid states"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    #[test]
    fn close_rejects_later_reservations_but_waits_for_earlier_one() {
        let gate = IngressGate::new();
        let permit = gate.reserve().expect("open ingress must grant a permit");

        assert_eq!(
            gate.begin_close(),
            IngressClose::Started {
                in_flight_reservations: 1
            }
        );
        assert_eq!(gate.state(), IngressState::Closing);
        assert!(matches!(
            gate.reserve(),
            Err(IngressReserveError::NotOpen(IngressState::Closing))
        ));
        assert_eq!(
            gate.try_seal(),
            Err(IngressSealError::ReservationsRemain(
                NonZeroUsize::new(1).expect("one reservation is non-zero")
            ))
        );

        permit.settle();
        assert_eq!(gate.try_seal(), Ok(IngressState::Sealed));
        assert_eq!(gate.state(), IngressState::Sealed);
        assert!(matches!(
            gate.reserve(),
            Err(IngressReserveError::NotOpen(IngressState::Sealed))
        ));
    }

    #[test]
    fn dropped_permit_unblocks_a_non_blocking_close() {
        let gate = IngressGate::new();
        let permit = gate.reserve().expect("open ingress must grant a permit");
        assert!(matches!(gate.begin_close(), IngressClose::Started { .. }));

        drop(permit);
        assert_eq!(gate.in_flight_reservations(), 0);
        assert_eq!(gate.try_seal(), Ok(IngressState::Sealed));
        assert_eq!(gate.begin_close(), IngressClose::AlreadySealed);
    }

    #[test]
    fn sealing_requires_close_to_begin() {
        let gate = IngressGate::new();
        assert_eq!(gate.try_seal(), Err(IngressSealError::StillOpen));
        assert_eq!(gate.state(), IngressState::Open);
    }

    #[test]
    fn close_racing_existing_producers_never_seals_early() {
        const PRODUCERS: usize = 8;

        let gate = Arc::new(IngressGate::new());
        let start = Arc::new(Barrier::new(PRODUCERS + 1));
        let release = Arc::new(Barrier::new(PRODUCERS + 1));
        let (reserved_sender, reserved_receiver) = mpsc::channel();
        let mut workers = Vec::with_capacity(PRODUCERS);

        for _ in 0..PRODUCERS {
            let gate = Arc::clone(&gate);
            let start = Arc::clone(&start);
            let release = Arc::clone(&release);
            let reserved_sender = reserved_sender.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                let permit = gate
                    .reserve()
                    .expect("barrier admits producers before close");
                reserved_sender
                    .send(())
                    .expect("test coordinator must await each producer");
                release.wait();
                permit.settle();
            }));
        }
        drop(reserved_sender);

        start.wait();
        for _ in 0..PRODUCERS {
            reserved_receiver
                .recv()
                .expect("each producer must reserve before close begins");
        }

        assert_eq!(
            gate.begin_close(),
            IngressClose::Started {
                in_flight_reservations: PRODUCERS
            }
        );
        assert_eq!(
            gate.try_seal(),
            Err(IngressSealError::ReservationsRemain(
                NonZeroUsize::new(PRODUCERS).expect("producer count is non-zero")
            ))
        );

        release.wait();
        for worker in workers {
            worker.join().expect("producer must not panic");
        }

        assert_eq!(gate.in_flight_reservations(), 0);
        assert_eq!(gate.try_seal(), Ok(IngressState::Sealed));
    }
}
