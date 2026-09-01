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
