// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};

/// Monotonic admission state for a bounded work queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkQueueState {
    /// Producers may acquire new submission reservations.
    Open,
    /// New reservations are rejected while issued reservations and drains settle.
    Closing,
    /// All queue state has reached terminal ownership.
    Closed,
}

/// One producer admission granted while a [`WorkQueueModel`] was open.
///
/// The token is one-shot. It must be published or abandoned before close can
/// finish. This model token carries no thread or engine authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmitReservation {
    sequence: NonZeroU64,
}

/// One active consumer drain in a [`WorkQueueModel`].
///
/// The model admits at most one drain at a time. It must finish before close
/// can move residual payloads into terminal ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DrainLease {
    sequence: NonZeroU64,
}

/// Failure to acquire a new [`SubmitReservation`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReserveError {
    /// Closing or closed state no longer admits work.
    NotOpen(WorkQueueState),
    /// The model cannot issue another unique reservation safely.
    ReservationSpaceExhausted,
}

/// Failure to settle a producer reservation.
#[derive(Debug, Eq, PartialEq)]
pub enum PublishError<T> {
    /// The reservation was already published, abandoned, or was never issued.
    StaleReservation(T),
    /// The fixed-capacity queue retained its existing payloads.
    Full(T),
}

/// Failure to abandon a producer reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbandonError {
    /// The reservation was already settled or was never issued.
    StaleReservation,
}

/// Failure to start or operate a consumer drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainError {
    /// No queued payload is available for a new drain.
    Empty,
    /// A drain is already active.
    Busy,
    /// Closing is complete and no further drain is legal.
    Closed,
    /// The supplied lease was already finished or was never issued.
    StaleLease,
    /// The model cannot issue another unique drain lease safely.
    LeaseSpaceExhausted,
}

/// Why [`WorkQueueModel::finish_close`] cannot yet transfer terminal ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseError {
    /// Close has not begun, or it completed already.
    InvalidState(WorkQueueState),
    /// A producer admitted before close still must publish or abandon work.
    ReservationsRemain(usize),
    /// One consumer drain still owns the queue.
    DrainActive,
}

/// Terminal result of attempting to finish a close.
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub enum CloseOutcome<T> {
    /// Closing completed and transferred every residual payload to the caller.
    Closed(Vec<T>),
    /// A previous call already completed close and transferred residual payloads.
    AlreadyClosed,
}

/// Pure model of bounded work admission, drain, and terminal ownership.
///
/// This type intentionally models ordering rather than concurrent atomics. A
/// future implementation must preserve these rules while allowing multiple
/// producers: close rejects later reservations, waits for earlier reservations
/// and the active drain, then transfers at most `capacity` residual payloads to
/// its close owner.
#[derive(Debug)]
pub struct WorkQueueModel<T> {
    capacity: NonZeroUsize,
    state: WorkQueueState,
    queue: VecDeque<T>,
    reservations: BTreeSet<NonZeroU64>,
    active_drain: Option<NonZeroU64>,
    next_reservation: Option<NonZeroU64>,
    next_lease: Option<NonZeroU64>,
}

impl<T> WorkQueueModel<T> {
    /// Creates an open queue with a fixed non-zero payload capacity.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            state: WorkQueueState::Open,
            queue: VecDeque::with_capacity(capacity.get()),
            reservations: BTreeSet::new(),
            active_drain: None,
            next_reservation: NonZeroU64::new(1),
            next_lease: NonZeroU64::new(1),
        }
    }

    /// Returns the fixed maximum number of retained payloads.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// Returns the current queue lifecycle state.
    #[must_use]
    pub const fn state(&self) -> WorkQueueState {
        self.state
    }

    /// Returns the number of payloads retained at this instant.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Returns whether no payload is currently retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Returns the number of producer reservations not yet settled.
    #[must_use]
    pub fn in_flight_reservations(&self) -> usize {
        self.reservations.len()
    }

    /// Begins one producer admission while the queue remains open.
    ///
    /// # Errors
    ///
    /// Returns [`ReserveError::NotOpen`] after close begins, or
    /// [`ReserveError::ReservationSpaceExhausted`] instead of recycling a
    /// reservation identity.
    pub fn reserve(&mut self) -> Result<SubmitReservation, ReserveError> {
        if self.state != WorkQueueState::Open {
            return Err(ReserveError::NotOpen(self.state));
        }
        let sequence = self
            .next_reservation
            .ok_or(ReserveError::ReservationSpaceExhausted)?;
        self.next_reservation = sequence.get().checked_add(1).and_then(NonZeroU64::new);
        let inserted = self.reservations.insert(sequence);
        debug_assert!(inserted, "reservation sequences are never reused");
        Ok(SubmitReservation { sequence })
    }

    /// Publishes one payload through an earlier [`SubmitReservation`].
    ///
    /// Reservations issued before [`Self::begin_close`] may still publish. The
    /// reservation settles regardless of whether publication succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::Full`] with the original payload when capacity
    /// is exhausted, or [`PublishError::StaleReservation`] when the token was
    /// already settled or unknown.
    pub fn publish(
        &mut self,
        reservation: SubmitReservation,
        payload: T,
    ) -> Result<(), PublishError<T>> {
        if !self.reservations.remove(&reservation.sequence) {
            return Err(PublishError::StaleReservation(payload));
        }
        if self.queue.len() == self.capacity.get() {
            return Err(PublishError::Full(payload));
        }
        self.queue.push_back(payload);
        Ok(())
    }

    /// Releases a reservation without publishing work.
    ///
    /// # Errors
    ///
    /// Returns [`AbandonError::StaleReservation`] when the token was already
    /// settled or unknown.
    pub fn abandon(&mut self, reservation: SubmitReservation) -> Result<(), AbandonError> {
        if self.reservations.remove(&reservation.sequence) {
            Ok(())
        } else {
            Err(AbandonError::StaleReservation)
        }
    }

    /// Stops admitting new reservations. Repeated requests are idempotent.
    pub fn begin_close(&mut self) {
        if self.state == WorkQueueState::Open {
            self.state = WorkQueueState::Closing;
        }
    }

    /// Begins the only active payload drain.
    ///
    /// Closing still allows a drain so a host can perform separately authorized
    /// final work before it claims residual terminal ownership.
    ///
    /// # Errors
    ///
    /// Returns [`DrainError::Empty`] when no payload exists,
    /// [`DrainError::Busy`] while another lease is active,
    /// [`DrainError::Closed`] after terminal close, or
    /// [`DrainError::LeaseSpaceExhausted`] instead of recycling a lease ID.
    pub fn begin_drain(&mut self) -> Result<DrainLease, DrainError> {
        if self.state == WorkQueueState::Closed {
            return Err(DrainError::Closed);
        }
        if self.active_drain.is_some() {
            return Err(DrainError::Busy);
        }
        if self.queue.is_empty() {
            return Err(DrainError::Empty);
        }
        let sequence = self.next_lease.ok_or(DrainError::LeaseSpaceExhausted)?;
        self.next_lease = sequence.get().checked_add(1).and_then(NonZeroU64::new);
        self.active_drain = Some(sequence);
        Ok(DrainLease { sequence })
    }

    /// Removes the next payload through an active [`DrainLease`].
    ///
    /// # Errors
    ///
    /// Returns [`DrainError::StaleLease`] unless `lease` is the currently live
    /// drain lease.
    pub fn pop(&mut self, lease: DrainLease) -> Result<Option<T>, DrainError> {
        self.require_active_drain(lease)?;
        Ok(self.queue.pop_front())
    }

    /// Ends an active drain.
    ///
    /// # Errors
    ///
    /// Returns [`DrainError::StaleLease`] unless `lease` is the currently live
    /// drain lease.
    pub fn finish_drain(&mut self, lease: DrainLease) -> Result<(), DrainError> {
        self.require_active_drain(lease)?;
        self.active_drain = None;
        Ok(())
    }

    /// Completes close after all pre-close producers and drains have settled.
    ///
    /// The returned payloads have terminal caller ownership. The queue does not
    /// execute them, invoke an engine, or choose a release policy.
    ///
    /// # Errors
    ///
    /// Returns [`CloseError::ReservationsRemain`] or [`CloseError::DrainActive`]
    /// while publication or draining is still possible, and
    /// [`CloseError::InvalidState`] when close has not begun.
    pub fn finish_close(&mut self) -> Result<CloseOutcome<T>, CloseError> {
        match self.state {
            WorkQueueState::Open => return Err(CloseError::InvalidState(self.state)),
            WorkQueueState::Closed => return Ok(CloseOutcome::AlreadyClosed),
            WorkQueueState::Closing => {}
        }
        if !self.reservations.is_empty() {
            return Err(CloseError::ReservationsRemain(self.reservations.len()));
        }
        if self.active_drain.is_some() {
            return Err(CloseError::DrainActive);
        }

        self.state = WorkQueueState::Closed;
        Ok(CloseOutcome::Closed(self.queue.drain(..).collect()))
    }

    fn require_active_drain(&self, lease: DrainLease) -> Result<(), DrainError> {
        if self.active_drain == Some(lease.sequence) {
            Ok(())
        } else {
            Err(DrainError::StaleLease)
        }
    }
}

impl fmt::Display for ReserveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOpen(state) => write!(formatter, "queue is not open: {state:?}"),
            Self::ReservationSpaceExhausted => formatter.write_str("reservation space exhausted"),
        }
    }
}

impl<T> fmt::Display for PublishError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleReservation(_) => formatter.write_str("submission reservation is stale"),
            Self::Full(_) => formatter.write_str("bounded work queue is full"),
        }
    }
}

impl fmt::Display for AbandonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("submission reservation is stale")
    }
}

impl fmt::Display for DrainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("bounded work queue is empty"),
            Self::Busy => formatter.write_str("a work drain is already active"),
            Self::Closed => formatter.write_str("bounded work queue is closed"),
            Self::StaleLease => formatter.write_str("work drain lease is stale"),
            Self::LeaseSpaceExhausted => formatter.write_str("drain lease space exhausted"),
        }
    }
}

impl fmt::Display for CloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(state) => write!(formatter, "cannot finish close from {state:?}"),
            Self::ReservationsRemain(count) => write!(formatter, "{count} reservations remain"),
            Self::DrainActive => formatter.write_str("a work drain remains active"),
        }
    }
}

impl Error for ReserveError {}

impl<T: fmt::Debug + 'static> Error for PublishError<T> {}

impl Error for AbandonError {}

impl Error for DrainError {}

impl Error for CloseError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test capacity must be non-zero")
    }

    #[test]
    fn close_rejects_later_reservations_and_returns_residual_payloads() {
        let mut queue = WorkQueueModel::new(capacity(2));
        let reservation = queue.reserve().unwrap();
        queue
            .publish(reservation, String::from("retained"))
            .unwrap();

        queue.begin_close();
        assert_eq!(queue.state(), WorkQueueState::Closing);
        assert_eq!(
            queue.reserve(),
            Err(ReserveError::NotOpen(WorkQueueState::Closing))
        );

        assert_eq!(
            queue.finish_close(),
            Ok(CloseOutcome::Closed(vec![String::from("retained")]))
        );
        assert_eq!(queue.state(), WorkQueueState::Closed);
        assert_eq!(queue.finish_close(), Ok(CloseOutcome::AlreadyClosed));
    }

    #[test]
    fn pre_close_reservation_can_publish_but_blocks_terminal_close() {
        let mut queue = WorkQueueModel::new(capacity(1));
        let reservation = queue.reserve().unwrap();
        queue.begin_close();

        assert_eq!(queue.finish_close(), Err(CloseError::ReservationsRemain(1)));
        queue.publish(reservation, 7_u32).unwrap();
        assert_eq!(queue.in_flight_reservations(), 0);
        assert_eq!(queue.finish_close(), Ok(CloseOutcome::Closed(vec![7])));
    }

    #[test]
    fn abandoning_pre_close_reservation_unblocks_terminal_close() {
        let mut queue = WorkQueueModel::<()>::new(capacity(1));
        let reservation = queue.reserve().unwrap();
        queue.begin_close();

        assert_eq!(queue.finish_close(), Err(CloseError::ReservationsRemain(1)));
        queue.abandon(reservation).unwrap();
        assert_eq!(queue.finish_close(), Ok(CloseOutcome::Closed(vec![])));
    }

    #[test]
    fn active_drain_blocks_close_and_residual_payloads_remain_owned_once() {
        let mut queue = WorkQueueModel::new(capacity(3));
        for payload in [1_u32, 2, 3] {
            let reservation = queue.reserve().unwrap();
            queue.publish(reservation, payload).unwrap();
        }
        let drain = queue.begin_drain().unwrap();
        assert_eq!(queue.pop(drain), Ok(Some(1)));
        queue.begin_close();

        assert_eq!(queue.finish_close(), Err(CloseError::DrainActive));
        queue.finish_drain(drain).unwrap();
        assert_eq!(queue.finish_close(), Ok(CloseOutcome::Closed(vec![2, 3])));
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn saturation_returns_payload_and_settles_its_reservation() {
        let mut queue = WorkQueueModel::new(capacity(1));
        let first = queue.reserve().unwrap();
        queue.publish(first, String::from("kept")).unwrap();
        let second = queue.reserve().unwrap();

        assert_eq!(
            queue.publish(second, String::from("returned")),
            Err(PublishError::Full(String::from("returned")))
        );
        assert_eq!(queue.in_flight_reservations(), 0);
        queue.begin_close();
        assert_eq!(
            queue.finish_close(),
            Ok(CloseOutcome::Closed(vec![String::from("kept")]))
        );
    }

    #[test]
    fn stale_tokens_do_not_retain_or_publish_payloads() {
        let mut queue = WorkQueueModel::new(capacity(1));
        let reservation = queue.reserve().unwrap();
        queue.abandon(reservation).unwrap();

        assert_eq!(
            queue.abandon(reservation),
            Err(AbandonError::StaleReservation)
        );
        assert_eq!(
            queue.publish(reservation, 9_u32),
            Err(PublishError::StaleReservation(9))
        );
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.in_flight_reservations(), 0);
    }
}
