# `rustjsi-testkit`

Deterministic tests, conformance cases, and benchmark fixtures for `RustJSI`.

Status: experimental, `0.0.0`, and unpublished.

The current model provides scoped primitive values, programmed evaluation
outcomes, generational strong roots, exact-owner external buffers, and a pure
host-lifecycle state machine. It also models bounded work-queue close ordering:
pre-close reservations settle before terminal close, and residual payloads
transfer to the close owner exactly once. It is designed for reproducible
failure and ordering tests.

Lifecycle fixtures consume the same owner-issued `AttachmentId` as real
backends. Replacement cycles preserve their logical runtime ID while advancing
the attachment epoch, so stale work and foreign runtimes exercise one shared
identity contract rather than test-only integers.

`tests/host_lifecycle_sequences.rs` drives 100,000 seeded sequences of 24
steps over three runtimes against an independent reference model. Sequences
replace engines with new epochs, abandon issued epochs before activation, keep
retired attachments reachable, and deliver queued work records to stale
epochs, other runtimes, and attachments that are draining, invalid, or
destroyed. Each record is checked by the lifecycle model and by a `ModelHost`
that is asked to lend its backend whenever the record names its attachment, so
the host's entry gate alone must refuse late work. After every step both must
match the reference state, entry count, monotonic state order,
single-occurrence terminal transitions, and number of backend loans. The
runtimes share no state and run on one thread; this is not a concurrency test,
and the lifecycle has no separate created-but-inactive state. Under Miri the
same test runs 40 sequences.

`ModelBackend::with_entry` lends a thread-affine backend adapter for testing
borrowed access. Direct scopes and borrowed entries share root IDs, queued
outcomes, and buffer ownership. Host fixtures supply their own admission and
invalidation policy.

Passing the model is not evidence that a real engine ABI, garbage collector,
exception boundary, or performance path is correct.
