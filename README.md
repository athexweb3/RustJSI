# RustJSI

RustJSI is an experimental Rust-native JavaScript interoperability project.
It aims to give Rust applications a safe, explicit way to host JavaScript
engines, expose Rust functionality to JavaScript, and build engine adapters
without treating C++ JSI, React Native, or any one engine as the core model.

## Status

This workspace is unpublished. No stable API or supported engine backend
exists; all crates are `0.0.0` with `publish = false`.

Current feasibility code includes a feature-gated direct integration with the
macOS system JavaScriptCore C API, an experimental source-linked backend
contract implemented by both a deterministic model and a host-authorized JSC
adapter, and a deterministic host lifecycle model. These are research artifacts,
not compatibility or production claims.

## Intended direction

RustJSI is being organized around five areas:

1. runtime-safe JavaScript value and lifetime handling;
2. engine backends with explicit capability boundaries;
3. host-controlled runtime ownership, scheduling, and lifecycle;
4. a Rust-first module authoring surface; and
5. generated bindings from a portable module description.

React Native is a potential integration target, not the definition of this
project and not part of this repository's initial public deliverable.

## Workspace layout

Packages live in short root directories; package names use the `rustjsi-`
prefix.

| Directory | Package | Intended role |
| --- | --- | --- |
| [`core/`](core/) | `rustjsi-core` | Safe semantic API |
| [`runtime/`](runtime/) | `rustjsi-runtime` | Runtime-owned state and services |
| [`host/`](host/) | `rustjsi-host` | Host lifecycle and JavaScript-thread entry |
| [`embed/`](embed/) | `rustjsi-embed` | Standalone embedding support |
| [`backend/`](backend/) | `rustjsi-backend` | Engine backend contract |
| [`backend-jsc/`](backend-jsc/) | `rustjsi-backend-jsc` | JavaScriptCore backend |
| [`abi/`](abi/) | `rustjsi-abi` | Optional foreign-function boundary |
| [`module/`](module/) | `rustjsi-module` | Module authoring vocabulary |
| [`ir/`](ir/) | `rustjsi-ir` | Module description format |
| [`codegen/`](codegen/) | `rustjsi-codegen` | Binding generation |
| [`testkit/`](testkit/) | `rustjsi-testkit` | Test and conformance support |

The workspace currently has no `crates/` directory, public facade crate, or
procedural macro crate.

## Development

- Rust: `1.98.0` (pinned in [`rust-toolchain.toml`](rust-toolchain.toml))
- Edition: Rust 2024
- MSRV policy: Rust 1.85
- License: MIT OR Apache-2.0

```text
just fmt-check
just check
just clippy
just test
just ci
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for local tooling and contribution
guidance. See [`ARCHITECTURE.md`](ARCHITECTURE.md), [`ROADMAP.md`](ROADMAP.md),
[`UNSAFE.md`](UNSAFE.md), and [`SECURITY.md`](SECURITY.md) for the public
project posture.
