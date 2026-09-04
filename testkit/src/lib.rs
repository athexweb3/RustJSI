// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic tests, conformance cases, and benchmark fixtures for `RustJSI`.
//!
//! The model backend makes ordering and failure paths reproducible. Passing this
//! model is necessary for a real backend, but does not prove an engine ABI, GC,
//! exception implementation, or performance characteristic.

#![forbid(unsafe_code)]

mod conformance;
mod lifecycle;
mod model;
mod model_entry;

pub use conformance::{
    create_number_root, verify_base_values, verify_external_buffer_identity,
    verify_external_buffer_identity_in_scope, verify_number_root_and_release,
    verify_owned_external_buffer, verify_strong_root_round_trip,
};
pub use lifecycle::{
    Entry, Epoch, LifecycleError, LifecycleEvent, LifecycleModel, RuntimeId, RuntimeState,
};
pub use model::{
    Evaluation, ExternalBufferStats, ModelBackend, ModelBufferView, ModelRoot, ModelScope,
    ModelValue, Primitive,
};
pub use model_entry::{ModelEntry, ModelEntryScope};
