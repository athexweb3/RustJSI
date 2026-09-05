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

Schedulers, cross-thread handles, and attached-engine synchronization adapters
are not implemented yet. Policy/outcome accounting does not grant engine access
or perform cleanup itself. The source-linked `Host` contract is not the stable C
Host ABI and does not complete the runtime-facing `Context` API.
