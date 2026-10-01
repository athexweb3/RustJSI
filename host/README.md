# `rustjsi-host`

Host lifecycle, thread entry, and scheduling for `RustJSI`.

Status: `0.0.0`, unpublished. `Host` is the experimental source-linked contract
for lending backend mechanics only while a host has established legal engine
entry. It carries attachment identity and lifecycle state but does not by itself
create a VM lock, scheduler, or engine lease. The higher-ranked entry closure
prevents backend adapters and scoped values from escaping.

`EntryGate` provides thread-affine entry accounting with a depth limit and
monotonic shutdown state:

```text
Active -> Draining -> Invalid -> Destroyed
```

New entries stop at `Draining`. Existing guards must leave before teardown.
Guard drop only updates the count, including during unwinding; it does not
destroy resources or call user code. A forgotten guard blocks teardown.

Each attachment declares whether a final legal engine entry is `Guaranteed`,
`BestEffort`, or `Unavailable`. After normal entries leave,
`try_begin_cleanup` acquires an exclusive cleanup guard without reopening normal
entry. Calling `complete` records successful entry-dependent cleanup. Dropping
the guard without completion, including during unwinding, leaves cleanup
retryable in `Draining`.

A guaranteed policy cannot finish draining until cleanup completes. Best-effort
and unavailable hosts may finish without final entry, and the gate records that
terminal outcome for diagnostics. The policy is fixed at gate construction;
the outcome is observed per attachment during teardown.

`TerminalResourceReport` provides fixed-size accounting for resources a backend
can classify at detach. Its `settled` ledger covers resources released or
retired by RustJSI, `unresolved` covers engine-dependent resources that could
not be settled without legal final entry, and `remaining` covers externally
owned buffers still live after detach. The ledger has explicit units for
persistent roots, callback registrations, native-state registrations, external
allocation count, and external bytes. It is neither an engine heap report nor a
process allocation report; an empty ledger says nothing about JavaScript
wrappers, temporary values, or other engine memory.

The gate does not establish engine-entry permission, VM locking, or runtime
identity. An enclosing host must do that before lending a backend, perform
cleanup when the gate reports drain-ready, and then record invalidation and
engine release. Gate operations contain no heap allocations or locks.

`RuntimeIdentity` allocates one logical runtime ID and issues monotonically
increasing attachment epochs as that host replaces engines. The issuer cannot
be cloned and callers cannot construct IDs from arbitrary integers. Cheap,
copyable `AttachmentId` snapshots let roots, backend state, and future queued
work reject another runtime or an earlier attachment. This allocator is shared
within one linked `rustjsi-host` domain; an eventual binary Host ABI must define
its own single identity authority.

`ScheduledWork<T>` is an identity-bound envelope for host-owned queues. It
captures an `AttachmentId`, validates it immediately before host entry, and
returns the original record on stale-attachment or entry rejection.
`ScheduledWorkMailbox<T>` is created for one immutable attachment epoch. It
adds fixed-capacity, close-aware retention and a single normal drain lease. A
drain can dispatch one record through the host at a time and repost a pending
successor with that same attachment. A retry post keeps normal admission until
the host accepts or rejects it, preventing terminal close from taking retained
work in that interval. It does not create a scheduler, retry policy, or engine
task.

`DrainRegistration` is the host owner's identity-only selector for
platform-delivered drain tasks. It represents one logical runtime's current
attachment as active or closing. Replacement requires closing state, the same
runtime ID, and a strictly newer epoch. Before any mailbox selection or host
entry it resolves a task as current, closing, retired, unregistered, or
foreign. It owns no queue or payload, so beginning close does not replace the
separate producer-close and terminal-drain protocol of
`ScheduledWorkMailbox<T>`. The registration remains thread-affine; producers
post copyable attachment identities and its host owner resolves them.

`ScheduledWorkMailbox<T>::state` exposes a concurrent lifecycle snapshot for
host diagnostics. Only its `Closed` state is terminally stable; an owner still
uses drain leases rather than a snapshot to coordinate payload transfer. The
`is_terminally_closed` predicate makes the terminal condition explicit for a
later replacement coordinator without creating one here.

When `T: Send`, producer threads may share `ScheduledWorkMailbox<T>` and retain
one coalesced attachment-only post. Its acquired `ScheduledWorkDrain` remains
thread-affine, so accepting work does not transfer runtime-consumer authority.

`AttachmentWorkOwner<T>` composes exactly one thread-affine
`DrainRegistration` with that current attachment's `ScheduledWorkMailbox<T>`.
Its cloneable `ScheduledWorkSender<T>` retains only immutable mailbox enqueue
and initial-post capabilities; it cannot resolve tasks, enter a backend, close
ingress, transfer terminal work, or replace the attachment. Closing first makes
the registration non-dispatchable, then stops mailbox admission. Replacement is
allowed only after the old mailbox reaches terminal `Closed`, at which point a
fresh mailbox and sender set target the newer attachment. Resolving a delivered
task may lend the active mailbox, but never grants host entry or scheduling.

Schedulers, cross-thread handles, and attached-engine synchronization adapters
are not implemented yet. Policy/outcome accounting does not grant engine access
or perform cleanup itself. The source-linked `Host` contract is not the stable C
Host ABI and does not complete the runtime-facing `Context` API.
