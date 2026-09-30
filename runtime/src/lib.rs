// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime state, roots, tasks, resources, and diagnostics for `RustJSI`.

#![forbid(unsafe_code)]

mod drain;

pub use drain::{
    DrainAcquire, DrainAfter, DrainClose, DrainPermit, DrainRequest, DrainSignal, DrainState,
};
