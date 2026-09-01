// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic tests, conformance cases, and benchmark fixtures for `RustJSI`.
//!
//! The model backend makes ordering and failure paths reproducible. Passing this
//! model is necessary for a real backend, but does not prove an engine ABI, GC,
//! exception implementation, or performance characteristic.

#![forbid(unsafe_code)]

mod lifecycle;
mod model;

pub use lifecycle::{
    Entry, Epoch, LifecycleError, LifecycleEvent, LifecycleModel, RuntimeId, RuntimeState,
};
pub use model::{
    Evaluation, ExternalBufferStats, ModelBackend, ModelBufferView, ModelRoot, ModelScope,
    ModelValue, Primitive,
};
