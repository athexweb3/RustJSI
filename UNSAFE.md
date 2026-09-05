# Unsafe code policy and inventory

Engine APIs, callbacks, raw backing stores, and foreign-function boundaries may
require contained `unsafe` code. Keep that surface small and reviewable.

## Current inventory

Safe contract and model crates forbid `unsafe`. Engine and foreign boundary
crates deny `unsafe_op_in_unsafe_fn`.

`rustjsi-backend` contains no unsafe code. Backend implementors keep raw engine
operations inside their own crate and satisfy the safe source-level contract.

`rustjsi-backend-jsc` contains a feature-gated experimental binding to the macOS
JavaScriptCore C API. Its unsafe surface is limited to raw declarations, calls,
callback argument views, active-entry recovery, class private-data access, and
native-object finalization. It also transfers boxed byte allocations to JSC
through its no-copy ArrayBuffer API and reclaims them through a panic-contained,
thread-independent C deallocator. Finalizers enqueue opaque generational tokens
only; they do not enter JavaScriptCore, lock, allocate, or destroy user state.
Rust state is reclaimed later on the runtime-owning thread.

The direct boundary benchmark has separate raw JSC bindings. Its function-root
guard borrows an owning context guard so unprotect precedes context release on
return or unwind. Result validation is outside the measurement timers.

The common JSC adapter exists only inside a host-authorized entry. GC-managed
values returned through its safe scope API are protected and registered until
scope teardown, including when Rust moves the handle itself to heap storage.
Scalar primitives avoid this root path. Strong roots balance independent
protect/unprotect pairs through instance-bound generational slots. Temporary
ArrayBuffer backing pointers are used only for immediate construction-time
validation and are never exposed as safe borrowed slices.

Both standalone JSC entry paths hold a safe host admission guard until their
local scopes and finalizer drains have returned. Invalidation checks that no
entry remains before releasing roots or the context. The gate supplies only
lifecycle accounting; engine thread and lifetime checks remain in the host.

The experimental JSC `Attachment` accepts a foreign `JSContextRef` only through
documented unsafe entry methods. The caller guarantees that every entry uses the
same live global context, legal thread, VM lock, and host synchronization.
`Attachment` canonicalizes callback contexts but cannot prove those host facts.
It neither stores nor retains the pointer and never releases the foreign
context. Explicit final-entry detach balances engine protections; detach without
entry reports unresolved protections. Its destructor never calls JSC.

The legacy JSC `Context` path now gives managed `Local` results independent
entry-owned protections too, including values resolved from persistent roots.
Context teardown releases these protections before the admission guard exits,
on normal return or unwind. Scalar results do not accumulate local roots.

Callback dispatch holds an `Rc` lease, not a registry borrow, while running
user code. Teardown removes registrations before destroying captures, contains
each unwinding drop panic, and continues cleanup. Callback, native-state, and
external-buffer unwind boundaries also guard panic-payload destruction:
[`catch_unwind` can return a payload whose destructor panics](https://doc.rust-lang.org/std/panic/fn.catch_unwind.html).
Ordinary payloads are reclaimed. If that destruction panics, the secondary
payload is deliberately forgotten; this can leak memory on each such failure.
Abort-mode panics, aborting hooks, and double panics during unwinding remain
fatal. Containing a panic does not restore arbitrary user state.

## Required documentation

Every future unsafe block must have a nearby explanation of:

```text
// SAFETY:
// - validated preconditions
// - pointer or handle provenance
// - lifetime and thread conditions
// - aliasing and initialization conditions
// - unwind and failure behavior
// - relevant test coverage or upstream contract
```

Every unsafe public item must document its safety requirements. “Required by
FFI” is not a complete safety argument.

## Review

Unsafe changes require dedicated review. Reviewers must examine the safe API,
ownership model, lifecycle, thread access, failure behavior, test coverage,
and any generated boundary code. Catching a crash does not repair an invalid
safety invariant.
