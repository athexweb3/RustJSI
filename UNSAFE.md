# Unsafe code policy and inventory

Engine APIs, callbacks, raw backing stores, and foreign-function boundaries may
require contained `unsafe` code. Keep that surface small and reviewable.

## Current inventory

No implementation `unsafe` code exists in this repository.

Safe crates forbid `unsafe`. The `backend`, `backend-jsc`, and `abi` crates are
reserved as future boundary crates and deny `unsafe_op_in_unsafe_fn`.

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
