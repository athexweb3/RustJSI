# `rustjsi-runtime`

Runtime state, roots, tasks, resources, and diagnostics for `RustJSI`.

Status: `0.0.0`, unpublished and experimental. The current API provides
coalesced drain signalling, a fixed-capacity mailbox, and a producer ingress
gate for non-blocking close admission. It does not yet provide runtime
ownership, a closable mailbox, task cancellation, or engine integration.

`IngressGate` is intentionally narrower than a work queue: it lets a close
owner prevent later producer reservations and wait for earlier reservations to
settle without blocking. It does not retain payloads, run a scheduler, or claim
terminal ownership of queued work.
