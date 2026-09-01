# Contributing

Thanks for considering a contribution to RustJSI.

The workspace is at an early scaffold stage. Please discuss new public APIs,
engine integrations, asynchronous runtimes, foreign-function layers, or major
dependencies before implementing them.

## Before you start

Read the public project material:

- [`README.md`](README.md)
- [`ARCHITECTURE.md`](ARCHITECTURE.md)
- [`ROADMAP.md`](ROADMAP.md)
- [`UNSAFE.md`](UNSAFE.md)
- [`SECURITY.md`](SECURITY.md)

Early contributions are most useful when they improve reproducibility, test
infrastructure, measurement tooling, public documentation, or a narrowly
agreed feasibility experiment.

## Development toolchain

- Rust `1.98.0`, pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
- Rust 2024 edition
- MSRV policy: Rust `1.85.0`

Recommended local tools:

| Tool | Purpose |
| --- | --- |
| `just` | Discoverable local commands |
| `cargo-nextest` | Workspace tests |
| `cargo-deny` | Dependency policy |
| `cargo-llvm-cov` | Coverage reports |
| `typos` | Spelling checks |
| `lychee` | Link checks |
| `zizmor` | GitHub Actions analysis |

## Commands

```text
just fmt
just fmt-check
just check
just clippy
just test
just test-doc
just doc
just deny
just coverage
just miri
just ci
just verify
```

Report exactly which checks you ran. A skipped check is not a passing check.

## Safety and performance

Safe crates forbid `unsafe`. Boundary crates may eventually need it for engine
or ABI integration; follow [`UNSAFE.md`](UNSAFE.md) when that happens.

Do not add performance claims without a reproducible workload, baseline,
measurement environment, and raw result artifact. Do not add dependencies just
because they may be useful later.

## Pull requests

Use the [pull request template](.github/PULL_REQUEST_TEMPLATE.md). Explain:

- the problem and scope;
- runtime, thread, lifetime, allocation, copy, and compatibility impact;
- unsafe or foreign-function impact;
- tests and documentation;
- whether generated code or public API would change.

Changes that alter safety boundaries or public contracts need a design
discussion before merge.

## License

By contributing, you agree that your contribution is licensed under the
project's MIT OR Apache-2.0 terms.
