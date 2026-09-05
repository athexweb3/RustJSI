# Architecture

RustJSI is a Rust-native JavaScript interoperability layer.

## Intended boundaries

```text
JavaScript application
        │
        ▼
host integration ─── owns runtime lifecycle, legal thread entry, scheduling
        │
        ▼
RustJSI runtime ─── values, roots, tasks, resources, error boundaries
        │
        ▼
engine backend ─── engine handles, calls, GC hooks, buffers, exceptions
        │
        ▼
JavaScript engine
```

A module-description and code-generation layer sits above the runtime. It
generates static bindings for ordinary calls.

## Design commitments

- Live JavaScript values will remain tied to their owning runtime and legal
  execution context.
- Persistence, copying, sharing, and transfer will be explicit operations.
- Hosts own runtime lifecycle and scheduling; backends implement engine
  mechanics.
- Source-linked hosts lend backend access through a closure so engine-bound
  state cannot escape the host-authorized entry.
- Engine capabilities will be declared rather than silently emulated.
- Unsafe engine and ABI work will stay confined to small boundary crates.
- Async work will operate on owned native data and return to JavaScript through
  the host scheduler.
- Performance claims require published measurement methodology and results.

## Crate roles

The root workspace is split by responsibility. Dependencies follow the same
direction as the runtime layers; the deterministic testkit currently consumes
the backend contract.

| Area | Crates |
| --- | --- |
| safe semantics | `rustjsi-core`, `rustjsi-runtime` |
| runtime ownership | `rustjsi-host`, `rustjsi-embed` |
| engine/foreign boundary | `rustjsi-backend`, `rustjsi-backend-jsc`, `rustjsi-abi` |
| module tooling | `rustjsi-module`, `rustjsi-ir`, `rustjsi-codegen` |
| validation | `rustjsi-testkit` |

## Public design process

Public-contract changes will be discussed through issues and design proposals.

For the current implementation stage, see [`ROADMAP.md`](ROADMAP.md),
[`CONTRIBUTING.md`](CONTRIBUTING.md), and [`UNSAFE.md`](UNSAFE.md).
