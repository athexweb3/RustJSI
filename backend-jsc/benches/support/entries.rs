// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared empty-entry workload ordering for timing and allocation probes.

#[derive(Clone, Copy)]
pub(crate) enum EntryWorkload {
    Gate,
    Common,
    Foreign,
}

impl EntryWorkload {
    pub(crate) const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Gate => ("gate", "host_gate_admit_and_exit"),
            Self::Common => ("common", "jsc_common_empty_entry"),
            Self::Foreign => ("foreign", "jsc_foreign_common_empty_entry"),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn selected_order() -> [EntryWorkload; 3] {
    let gate = EntryWorkload::Gate;
    let common = EntryWorkload::Common;
    let foreign = EntryWorkload::Foreign;
    match std::env::var("RUSTJSI_ENTRY_ORDER").as_deref() {
        Ok("gate,common,foreign") | Err(_) => [gate, common, foreign],
        Ok("gate,foreign,common") => [gate, foreign, common],
        Ok("common,gate,foreign") => [common, gate, foreign],
        Ok("common,foreign,gate") => [common, foreign, gate],
        Ok("foreign,gate,common") => [foreign, gate, common],
        Ok("foreign,common,gate") => [foreign, common, gate],
        Ok(value) => panic!("invalid RUSTJSI_ENTRY_ORDER: {value}"),
    }
}
