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
calls, and persistent resolution until its Context scope exits. Moving a local
into a Rust container or dropping its original persistent lease cannot remove that local
protection. Scalar primitives skip root storage. Both scope implementations
share inline storage for 16 roots, then spill to a vector.

Use `Context::with_scope` for short batches inside a long host entry. Each child
has separate root storage, released on return or unwind. Parent values stay
valid; child locals can't escape. Return owned data or use `persist` explicitly.
Up to 64 child scopes may nest, independently of host admission depth. The
common backend already supports sequential short `open_scope` lifetimes.

Scopes only limit local retention when callers keep their batches small. They
don't impose a root-count/heap quota, undo JS effects or native registrations,
or drain pending persistent releases and native finalizers. Those still follow
host-entry maintenance. Scope cleanup is separate from runtime admission.

`Runtime::new_with_root_limits(RootLimits { persistent_slots, local_roots })`
configures both budgets; each defaults to 4096. Local admission counts active
protections and in-flight result reservations across all scopes in the runtime.
Evaluation/calls reserve before JS execution; common externalization reserves
before accepting the owner. Rejection leaves that operation unexecuted and
returns rejected external bytes unchanged. Exceptions and unwind refund unused
reservations; scope cleanup returns committed slots.

Unknown-result operations require headroom even if they would return scalars.
Context refunds scalar reservations immediately. Common evaluation/resolution
keeps its existing protection for every result; direct common primitive
constructors don't reserve. Zero local capacity rejects evaluation, but those
primitive constructors still work. Neither budget covers total heap bytes,
temporary arguments, callback registrations or exception metadata.

Last `Persistent` lease drop marks its existing registry slot for release. It
doesn't allocate or call JSC. Both entry paths drain requests before user code
and after normal return; invalidation releases pending and live roots together.
Pending slots cannot resolve or be reused until drained. Explicit common-backend
roots still use `RootScope::release` inside an entry.

An idle runtime retains pending roots. Use an empty `with_context` or
`with_backend` entry to flush them. If user code unwinds, exit maintenance waits
until the next entry or invalidation. Pending links need no separate queue
allocation. Slots retain capacity, and draining a backlog has no time budget.
`Persistent` remains thread-affine.

`Runtime::new` limits persistent registry slots to 4096. Use
`Runtime::new_with_persistent_root_limit(limit)` to choose a different budget
at creation. Both entry APIs share it. Pending releases and generation-exhausted
slots count; reusable slots are used first. Exhaustion rejects `persist` before
adding an engine protection. Zero disables persistence, not local operations.
This limits slot count, not allocator bytes, lease clones or the JS heap.

Both standalone entry paths use `rustjsi-host` entry accounting. Teardown
rejects outstanding entries before releasing roots or the engine. Panic
unwinding restores the previous active runtime and releases admission counts;
it does not run engine destruction. The internal entry limit counts host
admissions, not JavaScript recursion or callbacks within an admitted frame.

Invalidation holds an exclusive cleanup guard while releasing roots and
callback captures. The guard closes before engine release; cleanup does not
lend a `Context` or replace the active runtime of an enclosing entry.

Dispatch leases the callback with an `Rc` before releasing the registry borrow.
Teardown detaches the registry and destroys each capture separately; an
unwinding destructor panic does not skip the remaining callbacks or engine
release. `Runtime::callback_drop_panics()` reports contained capture-drop
panics. Publication rollback follows the same cleanup path.

Each runtime retains at most 4,096 host-function registrations. Further
installation fails before creating a JSC function or running a global setter.
Failed publication frees its registration; dropping a `HostFunction` handle or
overwriting its global does not. Existing callbacks remain callable at the cap.
This experimental count limit is separate from `RootLimits`; it does not bound
capture sizes or the JavaScript heap. Unregistered callback arguments follow
normal Rust drop behavior on rejection.

The `callback_admission` bench measures registration, rejection at the cap and
invalidation with empty captures. Registration includes name conversion, map
growth, JSC allocation/protection and global assignment. Rejection includes the
result check; invalidation includes engine release. Results are batch means,
not tail latency or bounds for callbacks with arbitrary capture destructors.

`with_native_state` also takes a private operation lease before releasing the
registry borrow. Retirement invalidates the handle immediately; an admitted
operation can finish reading the old state even if its slot is reused. Final
lease destruction runs on the runtime thread with panic containment. A user
operation panic still propagates and doesn't roll back mutations inside `T`.
Only shared `&T` access is exposed, not general same-runtime reentry or mutable
object dispatch. The 4,096-entry limit bounds registered states, not bytes or
retired states still held by active operations.

Native-wrapper publication uses a temporary JSC root through setters and
exception conversion. Failed publication captures the exception before dropping
Rust state, then detaches the wrapper token during rollback.

Panic containment cannot recover abort-mode panics, aborting hooks, or double
panics during unwinding. If destroying a caught panic payload itself panics,
the second payload is deliberately leaked to avoid another destructor call.

Run `cargo bench -p rustjsi-backend-jsc --features experimental-jsc --bench root_release`
to measure last-lease drop and entry drain separately. Results are batch means,
not tail latency or application throughput. Each batch uses separate roots to
one JS object. Drop includes Rust lease/vector deallocation; drain includes host
entry, queue polling and JSC unprotect calls. Creation and protection are outside
the timers, and unrelated live roots remain installed during each case.

The `native_access` bench measures typed state reads inside one admitted entry.
It includes identity checks and the operation lease but excludes wrapper
creation, JS calls, retirement and finalizer work. Results are run means, not
tail-latency or application-performance claims.

The `local_scopes` bench compares entry-wide retention (`batch=0`) with short
batches. It includes evaluation and local-root cleanup, not engine creation.
It reports elapsed time, not retained heap size or GC latency guarantees.
