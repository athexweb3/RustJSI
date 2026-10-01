// SPDX-License-Identifier: MIT OR Apache-2.0

//! `JavaScriptCore` backend for `RustJSI`.

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(all(feature = "experimental-jsc", target_os = "macos"))]
mod experimental;
#[cfg(all(feature = "experimental-jsc", target_os = "macos"))]
mod sys;

#[cfg(all(feature = "experimental-jsc", target_os = "macos"))]
pub use experimental::{
    Attachment, Call, CallLimits, Context, DetachReport, ExternalBuffer, ExternalBufferLimits,
    HostError, HostFunction, InboundCallbackLimits, JsError, JsException, JscAttachedHost,
    JscBackend, JscBackendFamily, JscEntrySource, JscHostError, JscRoot, JscRuntimeLimits,
    JscScope, JscValue, Local, NativeObject, NativeStateInstallError, NativeStateLimits,
    Persistent, RootLimits, Runtime, RuntimeError, Value,
};
