// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared scalar round-trip ordering for the boundary timing probe.

#[derive(Clone, Copy)]
pub(crate) enum ScalarWorkload {
    Direct,
    Common,
}

impl ScalarWorkload {
    pub(crate) const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Direct => ("direct", "direct_jsc_scalar"),
            Self::Common => ("common", "rustjsi_common_scalar"),
        }
    }
}

pub(crate) fn selected_order() -> [ScalarWorkload; 2] {
    let direct = ScalarWorkload::Direct;
    let common = ScalarWorkload::Common;
    match std::env::var("RUSTJSI_SCALAR_ORDER").as_deref() {
        Ok("direct,common") | Err(_) => [direct, common],
        Ok("common,direct") => [common, direct],
        Ok(value) => panic!("invalid RUSTJSI_SCALAR_ORDER: {value}"),
    }
}
