# `rustjsi-backend-jsc`

JavaScriptCore backend for `RustJSI`.

Status: `0.0.0`, unpublished. The `experimental-jsc` feature enables a direct
macOS system-JSC feasibility prototype. It currently covers scoped and
persistent values, host callbacks, exception translation, and experimental
typed native state with runtime-thread reclamation. Rust-owned boxed bytes can
also be transferred into JSC ArrayBuffers without copying the payload at the
engine boundary, with bounded accounting and GC-driven deallocation. None of
these APIs are stable yet.

The feature also exposes an entry-borrowing implementation of the common
`rustjsi-backend` contract. The standalone `Runtime` owns lifecycle and lends a
short-lived `JscBackend` only during authorized entry. Common JSC scopes support
strict primitive reads, semantic value kinds, explicit generational roots, and
owned external buffers. They deliberately do not advertise borrowed buffer
bytes because JSC documents its backing-store pointer as temporary across API
calls.
