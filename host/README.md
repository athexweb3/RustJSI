# `rustjsi-host`

Host lifecycle, thread entry, and scheduling for `RustJSI`.

Status: `0.0.0`, unpublished. `EntryGate` provides experimental thread-affine
entry accounting with a depth limit and monotonic shutdown state:

```text
Active -> Draining -> Invalid -> Destroyed
```

New entries stop at `Draining`. Existing guards must leave before teardown.
Guard drop only updates the count, including during unwinding; it does not
destroy resources or call user code. A forgotten guard blocks teardown.

The gate does not establish engine-entry permission, VM locking, or runtime
identity. An enclosing host must do that before lending a backend, perform
cleanup when the gate reports drain-ready, and then record invalidation and
engine release. Gate operations contain no heap allocations or locks.

Schedulers, cross-thread handles, attached-engine leases, and host-authorized
final cleanup entries are not implemented yet.
