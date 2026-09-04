// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cell::Cell;
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::rc::Rc;

/// Monotonic state of one host attachment after initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostState {
    /// New entries may be admitted.
    Active,
    /// New entries are rejected; admitted entries must return before teardown.
    Draining,
    /// Entry-dependent cleanup has finished; engine access is no longer legal.
    Invalid,
    /// The host has released or detached the engine.
    Destroyed,
}

/// Host promise for a final legal engine entry during teardown.
///
/// This policy is fixed for one attachment. It describes host authority, not
/// whether a particular cleanup attempt has completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalEntryPolicy {
    /// The host promises one final legal entry before engine destruction.
    Guaranteed,
    /// The host will attempt a final entry but may have to finish without one.
    BestEffort,
    /// The host cannot provide a final engine entry during teardown.
    Unavailable,
}

/// Observed final-entry outcome when draining becomes invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalEntryOutcome {
    /// Host-authorized entry-dependent cleanup completed.
    Completed,
    /// Draining finished without a final engine entry.
    Unavailable,
}

/// A rejected entry or lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateError {
    /// The attachment no longer admits new entries.
    NotActive(HostState),
    /// The configured number of simultaneous entries has been reached.
    DepthLimit,
    /// Teardown must wait for this many entry guards.
    EntriesRemain(u32),
    /// An exclusive cleanup guard has not yet been dropped.
    CleanupInProgress,
    /// Guaranteed final-entry cleanup has not completed.
    FinalEntryRequired,
    /// The attachment declares that final engine entry is unavailable.
    FinalEntryUnavailable,
    /// Final-entry cleanup already completed for this attachment.
    FinalEntryComplete,
    /// The requested transition is not legal in the current state.
    InvalidTransition {
        /// State at rejection.
        state: HostState,
        /// Attempted transition.
        operation: &'static str,
    },
}

/// Thread-affine entry accounting for a host-owned attachment.
///
/// This gate does not own an engine, establish its thread/VM lock, or authorize
/// raw engine access. The host must establish those conditions separately before
/// lending a backend. There are no engine calls, callbacks, locks, or heap
/// allocations in gate operations.
///
/// Create a new gate only after attachment initialization succeeds. A gate is
/// never reactivated or reused for a replacement runtime. Identity, scheduling,
/// and final-entry policy belong to the enclosing host.
///
/// ```compile_fail
/// use rustjsi_host::EntryGate;
/// fn require_send<T: Send>() {}
/// require_send::<EntryGate>();
/// ```
///
/// ```compile_fail
/// use rustjsi_host::EntryGate;
/// fn require_sync<T: Sync>() {}
/// require_sync::<EntryGate>();
/// ```
#[derive(Debug)]
pub struct EntryGate {
    state: Cell<HostState>,
    entries: Cell<u32>,
    cleanup: Cell<bool>,
    final_entry: Cell<Option<FinalEntryOutcome>>,
    limit: NonZeroU32,
    final_entry_policy: FinalEntryPolicy,
    _affine: PhantomData<Rc<()>>,
}

/// One counted entry; dropping it only releases its admission count.
///
/// It never runs teardown or invokes user code, including during unwinding.
/// Forgetting a guard leaves the gate permanently busy rather than allowing
/// premature teardown. This is accounting, not a backend-access token.
///
/// ```compile_fail
/// use rustjsi_host::{EntryGate, FinalEntryPolicy};
/// use std::num::NonZeroU32;
/// let guard = {
///     let gate = EntryGate::new(
///         NonZeroU32::new(8).unwrap(),
///         FinalEntryPolicy::BestEffort,
///     );
///     gate.try_enter().unwrap()
/// };
/// drop(guard);
/// ```
///
/// ```compile_fail
/// use rustjsi_host::EntryGuard;
/// fn require_send<T: Send>() {}
/// require_send::<EntryGuard<'static>>();
/// ```
#[derive(Debug)]
#[must_use = "dropping the guard exits the counted entry"]
pub struct EntryGuard<'gate> {
    gate: &'gate EntryGate,
}

/// Exclusive accounting for host-authorized cleanup during draining.
///
/// The host must separately establish engine ownership, thread and VM access.
/// This guard cannot admit application work. [`Self::complete`] records successful
/// entry-dependent cleanup. Drop without completion leaves the state Draining,
/// allowing a host to retry after unwind. Forgetting it blocks completion.
///
/// ```compile_fail
/// use rustjsi_host::CleanupGuard;
/// fn require_send<T: Send>() {}
/// require_send::<CleanupGuard<'static>>();
/// ```
///
/// ```compile_fail
/// use rustjsi_host::EntryGate;
/// use std::num::NonZeroU32;
/// let guard = {
///     use rustjsi_host::FinalEntryPolicy;
///     let gate = EntryGate::new(
///         NonZeroU32::new(1).unwrap(),
///         FinalEntryPolicy::Guaranteed,
///     );
///     gate.request_drain();
///     gate.try_begin_cleanup().unwrap()
/// };
/// drop(guard);
/// ```
#[derive(Debug)]
#[must_use = "keep the guard alive for the entire host cleanup entry"]
pub struct CleanupGuard<'gate> {
    gate: &'gate EntryGate,
}

impl EntryGate {
    /// Creates an active gate with explicit entry and teardown policy.
    #[must_use]
    pub const fn new(limit: NonZeroU32, final_entry_policy: FinalEntryPolicy) -> Self {
        Self {
            state: Cell::new(HostState::Active),
            entries: Cell::new(0),
            cleanup: Cell::new(false),
            final_entry: Cell::new(None),
            limit,
            final_entry_policy,
            _affine: PhantomData,
        }
    }

    /// Returns the current attachment state.
    #[must_use]
    pub fn state(&self) -> HostState {
        self.state.get()
    }

    /// Returns the number of normal entry guards not yet dropped.
    #[must_use]
    pub fn active_entries(&self) -> u32 {
        self.entries.get()
    }

    /// Returns this attachment's immutable final-entry policy.
    #[must_use]
    pub const fn final_entry_policy(&self) -> FinalEntryPolicy {
        self.final_entry_policy
    }

    /// Returns the terminal final-entry outcome once cleanup is completed or
    /// draining finishes without an entry.
    #[must_use]
    pub fn final_entry_outcome(&self) -> Option<FinalEntryOutcome> {
        self.final_entry.get()
    }

    /// Counts a new entry after the host has checked engine entry legality.
    ///
    /// The limit bounds simultaneous guards, not JavaScript recursion, engine
    /// stack depth, or Rust callbacks already covered by an enclosing entry.
    ///
    /// # Errors
    ///
    /// Rejects non-active state and the configured depth limit without mutation.
    pub fn try_enter(&self) -> Result<EntryGuard<'_>, GateError> {
        if self.state.get() != HostState::Active {
            return Err(GateError::NotActive(self.state.get()));
        }
        let entries = self.entries.get();
        if entries >= self.limit.get() {
            return Err(GateError::DepthLimit);
        }
        self.entries.set(entries + 1);
        Ok(EntryGuard { gate: self })
    }

    /// Stops admitting entries. Repeated requests have no effect.
    ///
    /// Admitted frames retain their engine lease until they return. The host
    /// must not release the engine or its entry-dependent state before then.
    pub fn request_drain(&self) {
        if self.state.get() == HostState::Active {
            self.state.set(HostState::Draining);
        }
    }

    /// Reports whether normal entries have drained and no cleanup guard is live.
    ///
    /// The host may then attempt a policy-permitted cleanup entry or make its
    /// terminal drain decision. This predicate does not authorize engine access;
    /// `try_begin_cleanup` applies the attachment's final-entry policy.
    #[must_use]
    pub fn is_drain_ready(&self) -> bool {
        self.state.get() == HostState::Draining && self.entries.get() == 0 && !self.cleanup.get()
    }

    /// Reports whether a cleanup guard is live, independently of normal entries.
    #[must_use]
    pub fn cleanup_in_progress(&self) -> bool {
        self.cleanup.get()
    }

    /// Counts one exclusive cleanup entry after host authorization.
    ///
    /// This is optional accounting: hosts without an engine cleanup entry can
    /// still record their own terminal cleanup through `finish_drain`.
    ///
    /// # Errors
    ///
    /// Requires Draining, no normal entries and no outstanding cleanup guard.
    pub fn try_begin_cleanup(&self) -> Result<CleanupGuard<'_>, GateError> {
        if self.state.get() != HostState::Draining {
            return Err(GateError::InvalidTransition {
                state: self.state.get(),
                operation: "begin_cleanup",
            });
        }
        if self.entries.get() != 0 {
            return Err(GateError::EntriesRemain(self.entries.get()));
        }
        if self.final_entry_policy == FinalEntryPolicy::Unavailable {
            return Err(GateError::FinalEntryUnavailable);
        }
        if self.final_entry.get().is_some() {
            return Err(GateError::FinalEntryComplete);
        }
        if self.cleanup.get() {
            return Err(GateError::CleanupInProgress);
        }
        self.cleanup.set(true);
        Ok(CleanupGuard { gate: self })
    }

    /// Records completed entry-dependent cleanup and forbids further access.
    ///
    /// Call only after host-authorized cleanup, before releasing the engine.
    /// Repeating this transition in `Invalid` is harmless.
    ///
    /// # Errors
    ///
    /// Rejects live entry or cleanup guards and states other than `Draining` or `Invalid`.
    pub fn finish_drain(&self) -> Result<FinalEntryOutcome, GateError> {
        if self.cleanup.get() {
            return Err(GateError::CleanupInProgress);
        }
        match self.state.get() {
            HostState::Invalid => Ok(self
                .final_entry
                .get()
                .unwrap_or(FinalEntryOutcome::Unavailable)),
            HostState::Draining if self.entries.get() == 0 => {
                if self.final_entry_policy == FinalEntryPolicy::Guaranteed
                    && self.final_entry.get().is_none()
                {
                    return Err(GateError::FinalEntryRequired);
                }
                let outcome = self
                    .final_entry
                    .get()
                    .unwrap_or(FinalEntryOutcome::Unavailable);
                self.final_entry.set(Some(outcome));
                self.state.set(HostState::Invalid);
                Ok(outcome)
            }
            HostState::Draining => Err(GateError::EntriesRemain(self.entries.get())),
            state => Err(GateError::InvalidTransition {
                state,
                operation: "finish_drain",
            }),
        }
    }

    /// Records engine release or detachment after invalidation.
    ///
    /// This does not release any engine resource itself. It is idempotent once
    /// the attachment has reached `Destroyed`.
    ///
    /// # Errors
    ///
    /// Rejects states other than `Invalid` or `Destroyed`.
    pub fn mark_destroyed(&self) -> Result<(), GateError> {
        match self.state.get() {
            HostState::Destroyed => Ok(()),
            HostState::Invalid => {
                self.state.set(HostState::Destroyed);
                Ok(())
            }
            state => Err(GateError::InvalidTransition {
                state,
                operation: "destroy",
            }),
        }
    }
}

impl Drop for EntryGuard<'_> {
    fn drop(&mut self) {
        self.gate.entries.set(self.gate.entries.get() - 1);
    }
}

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        self.gate.cleanup.set(false);
    }
}

impl CleanupGuard<'_> {
    /// Records successful entry-dependent cleanup and releases the guard.
    ///
    /// This method does not validate engine ownership or thread legality. The
    /// host must establish those conditions before beginning cleanup.
    pub fn complete(self) {
        self.gate
            .final_entry
            .set(Some(FinalEntryOutcome::Completed));
    }
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotActive(state) => write!(formatter, "host is not active: {state:?}"),
            Self::DepthLimit => formatter.write_str("host entry depth limit reached"),
            Self::EntriesRemain(count) => write!(formatter, "{count} host entries remain"),
            Self::CleanupInProgress => formatter.write_str("host cleanup entry remains"),
            Self::FinalEntryRequired => {
                formatter.write_str("guaranteed final host entry has not completed")
            }
            Self::FinalEntryUnavailable => {
                formatter.write_str("host does not provide a final engine entry")
            }
            Self::FinalEntryComplete => {
                formatter.write_str("host final-entry cleanup already completed")
            }
            Self::InvalidTransition { state, operation } => {
                write!(formatter, "cannot {operation} while host is {state:?}")
            }
        }
    }
}

impl Error for GateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(limit: u32) -> EntryGate {
        EntryGate::new(
            NonZeroU32::new(limit).unwrap(),
            FinalEntryPolicy::BestEffort,
        )
    }

    fn guaranteed_gate(limit: u32) -> EntryGate {
        EntryGate::new(
            NonZeroU32::new(limit).unwrap(),
            FinalEntryPolicy::Guaranteed,
        )
    }

    #[test]
    fn nested_entries_defer_invalidation() {
        let gate = gate(2);
        let outer = gate.try_enter().unwrap();
        let inner = gate.try_enter().unwrap();
        gate.request_drain();
        gate.request_drain();
        assert_eq!(
            gate.try_enter().unwrap_err(),
            GateError::NotActive(HostState::Draining)
        );
        assert_eq!(gate.finish_drain(), Err(GateError::EntriesRemain(2)));
        drop(inner);
        assert!(!gate.is_drain_ready());
        assert_eq!(gate.finish_drain(), Err(GateError::EntriesRemain(1)));
        drop(outer);
        assert!(gate.is_drain_ready());
        gate.finish_drain().unwrap();
        gate.finish_drain().unwrap();
        gate.mark_destroyed().unwrap();
        gate.mark_destroyed().unwrap();
        gate.request_drain();
        assert_eq!(gate.state(), HostState::Destroyed);
        assert!(!gate.is_drain_ready());
        assert!(gate.try_enter().is_err());
    }

    #[test]
    fn depth_limit_rejects_without_changing_count() {
        let gate = gate(1);
        let entry = gate.try_enter().unwrap();
        assert_eq!(gate.try_enter().unwrap_err(), GateError::DepthLimit);
        assert_eq!(gate.active_entries(), 1);
        drop(entry);
        drop(gate.try_enter().unwrap());
        assert_eq!(gate.active_entries(), 0);
    }

    #[test]
    fn admission_counter_cannot_wrap() {
        let gate = gate(u32::MAX);
        gate.entries.set(u32::MAX);
        assert_eq!(gate.try_enter().unwrap_err(), GateError::DepthLimit);
        assert_eq!(gate.active_entries(), u32::MAX);
    }

    #[test]
    fn unwind_releases_entries_without_advancing_lifecycle() {
        let gate = gate(2);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _outer = gate.try_enter().unwrap();
            let _inner = gate.try_enter().unwrap();
            gate.request_drain();
            panic!("entry panic");
        }));
        assert!(result.is_err());
        assert!(gate.is_drain_ready());
        assert_eq!(gate.state(), HostState::Draining);
    }

    #[test]
    fn forgotten_guard_fails_closed() {
        let gate = gate(1);
        std::mem::forget(gate.try_enter().unwrap());
        gate.request_drain();
        assert!(!gate.is_drain_ready());
        assert_eq!(gate.finish_drain(), Err(GateError::EntriesRemain(1)));
    }

    #[test]
    fn out_of_order_guard_drop_cannot_enable_early_teardown() {
        let gate = gate(2);
        let outer = gate.try_enter().unwrap();
        let inner = gate.try_enter().unwrap();
        drop(outer);
        gate.request_drain();
        assert_eq!(gate.finish_drain(), Err(GateError::EntriesRemain(1)));
        drop(inner);
        gate.finish_drain().unwrap();
    }

    #[test]
    fn invalid_transitions_do_not_change_state() {
        let gate = gate(1);
        assert!(gate.finish_drain().is_err());
        assert!(gate.mark_destroyed().is_err());
        assert_eq!(gate.state(), HostState::Active);
        gate.request_drain();
        assert!(gate.mark_destroyed().is_err());
        assert_eq!(gate.state(), HostState::Draining);
        gate.finish_drain().unwrap();
        assert!(gate.try_enter().is_err());
        gate.mark_destroyed().unwrap();
        assert!(gate.finish_drain().is_err());
        assert_eq!(gate.state(), HostState::Destroyed);
    }

    #[test]
    fn gates_are_independent() {
        let first = gate(1);
        let second = gate(1);
        let _entry = first.try_enter().unwrap();
        second.request_drain();
        second.finish_drain().unwrap();
        second.mark_destroyed().unwrap();
        assert_eq!(first.state(), HostState::Active);
        assert_eq!(first.active_entries(), 1);
    }

    #[test]
    fn cleanup_is_exclusive_and_cannot_complete_while_live() {
        let gate = gate(1);
        assert!(gate.try_begin_cleanup().is_err());
        let entry = gate.try_enter().unwrap();
        gate.request_drain();
        assert_eq!(
            gate.try_begin_cleanup().unwrap_err(),
            GateError::EntriesRemain(1)
        );
        drop(entry);
        let cleanup = gate.try_begin_cleanup().unwrap();
        assert_eq!(gate.active_entries(), 0);
        assert!(gate.cleanup_in_progress());
        assert!(!gate.is_drain_ready());
        assert!(gate.try_enter().is_err());
        assert_eq!(
            gate.try_begin_cleanup().unwrap_err(),
            GateError::CleanupInProgress
        );
        assert_eq!(gate.finish_drain(), Err(GateError::CleanupInProgress));
        assert!(gate.mark_destroyed().is_err());
        drop(cleanup);
        gate.finish_drain().unwrap();
        assert!(gate.try_begin_cleanup().is_err());
        gate.mark_destroyed().unwrap();
        assert!(gate.try_begin_cleanup().is_err());
    }

    #[test]
    fn cleanup_unwind_leaves_a_retryable_draining_gate() {
        let gate = gate(1);
        gate.request_drain();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _cleanup = gate.try_begin_cleanup().unwrap();
            panic!("cleanup failed");
        }));
        assert!(result.is_err());
        assert!(gate.is_drain_ready());
        assert_eq!(gate.state(), HostState::Draining);
        drop(gate.try_begin_cleanup().unwrap());
        gate.finish_drain().unwrap();
    }

    #[test]
    fn forgotten_cleanup_guard_blocks_teardown() {
        let gate = gate(1);
        gate.request_drain();
        std::mem::forget(gate.try_begin_cleanup().unwrap());
        assert_eq!(gate.finish_drain(), Err(GateError::CleanupInProgress));
        assert!(gate.try_begin_cleanup().is_err());
        assert!(!gate.is_drain_ready());
    }

    #[test]
    fn guaranteed_policy_requires_completed_cleanup() {
        let gate = guaranteed_gate(1);
        assert_eq!(gate.final_entry_policy(), FinalEntryPolicy::Guaranteed);
        gate.request_drain();
        assert!(gate.is_drain_ready());
        assert_eq!(gate.finish_drain(), Err(GateError::FinalEntryRequired));

        let cleanup = gate.try_begin_cleanup().unwrap();
        drop(cleanup);
        assert_eq!(gate.finish_drain(), Err(GateError::FinalEntryRequired));

        gate.try_begin_cleanup().unwrap().complete();
        assert_eq!(
            gate.final_entry_outcome(),
            Some(FinalEntryOutcome::Completed)
        );
        assert_eq!(gate.finish_drain(), Ok(FinalEntryOutcome::Completed));
        assert_eq!(gate.finish_drain(), Ok(FinalEntryOutcome::Completed));
    }

    #[test]
    fn best_effort_and_unavailable_report_missing_final_entry() {
        let best_effort = gate(1);
        best_effort.request_drain();
        assert_eq!(
            best_effort.finish_drain(),
            Ok(FinalEntryOutcome::Unavailable)
        );
        assert_eq!(
            best_effort.final_entry_outcome(),
            Some(FinalEntryOutcome::Unavailable)
        );

        let unavailable =
            EntryGate::new(NonZeroU32::new(1).unwrap(), FinalEntryPolicy::Unavailable);
        unavailable.request_drain();
        assert!(unavailable.is_drain_ready());
        assert_eq!(
            unavailable.try_begin_cleanup().unwrap_err(),
            GateError::FinalEntryUnavailable
        );
        assert_eq!(
            unavailable.finish_drain(),
            Ok(FinalEntryOutcome::Unavailable)
        );
    }

    #[test]
    fn completed_cleanup_cannot_be_reopened() {
        let gate = guaranteed_gate(1);
        gate.request_drain();
        gate.try_begin_cleanup().unwrap().complete();
        assert_eq!(
            gate.try_begin_cleanup().unwrap_err(),
            GateError::FinalEntryComplete
        );
        assert_eq!(gate.finish_drain(), Ok(FinalEntryOutcome::Completed));
    }
}
