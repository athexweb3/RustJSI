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

The older `Context` API also roots managed locals returned by evaluation,
calls, and persistent resolution until entry exit. Moving a local into a Rust
container or dropping its original persistent lease cannot remove that local
protection. Scalar primitives skip root storage. Both scope implementations
share inline storage for 16 roots, then spill to a vector. Long object-heavy
entries still need bounded/nested frames before production use.

Both standalone entry paths use `rustjsi-host` entry accounting. Teardown
rejects outstanding entries before releasing roots or the engine. Panic
unwinding restores the previous active runtime and releases admission counts;
it does not run engine destruction. The internal entry limit counts host
admissions, not JavaScript recursion or callbacks within an admitted frame.

Dispatch leases the callback with an `Rc` before releasing the registry borrow.
Teardown detaches the registry and destroys each capture separately; an
unwinding destructor panic does not skip the remaining callbacks or engine
release. `Runtime::callback_drop_panics()` reports contained capture-drop
panics. Publication rollback follows the same cleanup path.

Panic containment cannot recover abort-mode panics, aborting hooks, or double
panics during unwinding. If destroying a caught panic payload itself panics,
the second payload is deliberately leaked to avoid another destructor call.
