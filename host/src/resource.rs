// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::FinalEntryOutcome;

/// A fixed-size observation of long-lived resources at one lifecycle boundary.
///
/// Each field has an explicit unit. This is not an engine heap measurement,
/// process allocator measurement, or estimate of JavaScript wrapper count. A
/// backend reports only the resources it directly tracks and has authority to
/// classify at terminal cleanup.
///
/// The ledger deliberately has no dynamic entries. Extending it requires a
/// contract revision instead of silently accepting a backend-specific metric
/// with an unspecified unit or consistency guarantee.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceLedger {
    persistent_roots: usize,
    callback_registrations: usize,
    native_states: usize,
    external_buffer_allocations: usize,
    external_buffer_bytes: usize,
}

impl ResourceLedger {
    /// Creates a ledger with explicit resource counts and byte units.
    #[must_use]
    pub const fn new(
        persistent_roots: usize,
        callback_registrations: usize,
        native_states: usize,
        external_buffer_allocations: usize,
        external_buffer_bytes: usize,
    ) -> Self {
        Self {
            persistent_roots,
            callback_registrations,
            native_states,
            external_buffer_allocations,
            external_buffer_bytes,
        }
    }

    /// Returns the number of backend persistent roots in this observation.
    #[must_use]
    pub const fn persistent_roots(self) -> usize {
        self.persistent_roots
    }

    /// Returns retained callback-registration identities in this observation.
    #[must_use]
    pub const fn callback_registrations(self) -> usize {
        self.callback_registrations
    }

    /// Returns live native-state registration identities in this observation.
    #[must_use]
    pub const fn native_states(self) -> usize {
        self.native_states
    }

    /// Returns externally owned buffer allocations in this observation.
    #[must_use]
    pub const fn external_buffer_allocations(self) -> usize {
        self.external_buffer_allocations
    }

    /// Returns bytes held by externally owned buffers in this observation.
    #[must_use]
    pub const fn external_buffer_bytes(self) -> usize {
        self.external_buffer_bytes
    }
}

/// Structured terminal resource accounting for one attachment.
///
/// `settled` records resources released or retired by `RustJSI` during the
/// detach operation. `unresolved` records engine-dependent resources that
/// could not be settled because the host finished without legal final entry.
/// `remaining` records still-live externally owned buffers after detach; their
/// owner, rather than this report, controls their eventual release.
///
/// Empty fields do not imply that an engine or process has no other retained
/// memory. The report is only a bounded observation of the resource classes in
/// [`ResourceLedger`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalResourceReport {
    final_entry: FinalEntryOutcome,
    settled: ResourceLedger,
    unresolved: ResourceLedger,
    remaining: ResourceLedger,
}

impl TerminalResourceReport {
    /// Creates terminal resource accounting with explicit lifecycle outcome.
    #[must_use]
    pub const fn new(
        final_entry: FinalEntryOutcome,
        settled: ResourceLedger,
        unresolved: ResourceLedger,
        remaining: ResourceLedger,
    ) -> Self {
        Self {
            final_entry,
            settled,
            unresolved,
            remaining,
        }
    }

    /// Returns the gate's observed final-entry outcome.
    #[must_use]
    pub const fn final_entry(self) -> FinalEntryOutcome {
        self.final_entry
    }

    /// Returns resources settled by this detach operation.
    #[must_use]
    pub const fn settled(self) -> ResourceLedger {
        self.settled
    }

    /// Returns engine-dependent resources left unresolved at detach.
    #[must_use]
    pub const fn unresolved(self) -> ResourceLedger {
        self.unresolved
    }

    /// Returns externally owned resources still live after detach.
    #[must_use]
    pub const fn remaining(self) -> ResourceLedger {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ledger_has_no_reported_resources() {
        assert_eq!(
            ResourceLedger::default(),
            ResourceLedger::new(0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn terminal_report_preserves_each_observation_boundary() {
        let settled = ResourceLedger::new(2, 3, 5, 0, 0);
        let unresolved = ResourceLedger::new(7, 11, 0, 0, 0);
        let remaining = ResourceLedger::new(0, 0, 0, 13, 17);
        let report = TerminalResourceReport::new(
            FinalEntryOutcome::Unavailable,
            settled,
            unresolved,
            remaining,
        );

        assert_eq!(report.final_entry(), FinalEntryOutcome::Unavailable);
        assert_eq!(report.settled(), settled);
        assert_eq!(report.unresolved(), unresolved);
        assert_eq!(report.remaining(), remaining);
    }

    #[test]
    fn ledger_accessors_keep_counts_and_bytes_distinct() {
        let ledger = ResourceLedger::new(1, 2, 3, 5, 8);

        assert_eq!(ledger.persistent_roots(), 1);
        assert_eq!(ledger.callback_registrations(), 2);
        assert_eq!(ledger.native_states(), 3);
        assert_eq!(ledger.external_buffer_allocations(), 5);
        assert_eq!(ledger.external_buffer_bytes(), 8);
    }
}
