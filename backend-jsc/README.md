# `rustjsi-backend-jsc`

JavaScriptCore backend for `RustJSI`.

Status: `0.0.0`, unpublished. The `experimental-jsc` feature enables a direct
macOS system-JSC feasibility prototype. It currently covers scoped and
persistent values, host callbacks, exception translation, and experimental
typed native state with runtime-thread reclamation. Rust-owned boxed bytes can
also be transferred into JSC ArrayBuffers without copying the payload at the
engine boundary, with bounded accounting and GC-driven deallocation. None of
these APIs are stable yet.
