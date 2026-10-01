# `rustjsi-testkit`

Deterministic tests, conformance cases, and benchmark fixtures for `RustJSI`.

Status: experimental, `0.0.0`, and unpublished.

The current model provides scoped primitive values, programmed evaluation
outcomes, generational strong roots, exact-owner external buffers, and a pure
host-lifecycle state machine. It also models bounded work-queue close ordering:
pre-close reservations and normal drains settle before a terminal owner can
transfer residual payloads one at a time. An unfinished terminal owner preserves
the residual work without reopening ordinary drains. It is designed for
reproducible failure and ordering tests.

Lifecycle fixtures consume the same owner-issued `AttachmentId` as real
backends. Replacement cycles preserve their logical runtime ID while advancing
the attachment epoch, so stale work and foreign runtimes exercise one shared
identity contract rather than test-only integers.

`DrainPostQueue` is a fixed-capacity deterministic `DrainPoster`. Each accepted
post retains only its `AttachmentId`; it cannot carry a backend borrow, a work
payload, or a host-entry callback. A test driver pulls a post, chooses the
matching host mailbox, and separately performs legal dispatch. The queue does
not deduplicate attachment records, validate lifecycle, create a platform
wake-up, or establish a host retry policy. Its purpose is to make bounded post
acceptance, saturation, and retry handoff reproducible.

Producers may share this queue across threads because a post carries only an
inert attachment identity. Its acquired drain remains thread-affine and is not
an engine entry capability; a driver must finish it on its chosen consumer
thread before separately validating attachment and host entry.

If a driver unwinds with an unfinished drain, retained post records remain
pending for a later deterministic recovery drain. The fixture exposes that
state but does not create a platform wake-up or retry policy.

`rustjsi_host::DrainRegistration` represents the host owner's decision about an
attachment-only drain task for one logical runtime. Its current attachment is
either active or closing. Replacement requires closing state, the same runtime
ID, and a strictly newer attachment epoch. Resolution distinguishes current,
closing, retired, unregistered, and foreign tasks before any host entry. It
does not provide a scheduler, backend attachment, wake-up mechanism, or
platform integration.

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
