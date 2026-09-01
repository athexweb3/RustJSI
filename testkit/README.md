# `rustjsi-testkit`

Deterministic tests, conformance cases, and benchmark fixtures for `RustJSI`.

Status: experimental, `0.0.0`, and unpublished.

The current model provides scoped primitive values, programmed evaluation
outcomes, generational strong roots, exact-owner external buffers, and a pure
host-lifecycle state machine. It is designed for reproducible failure and
ordering tests.

Passing the model is not evidence that a real engine ABI, garbage collector,
exception boundary, or performance path is correct.
