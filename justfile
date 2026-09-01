# SPDX-License-Identifier: MIT OR Apache-2.0

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

check:
    cargo check --workspace --all-targets --locked

clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

test:
    cargo nextest run --workspace --all-features --locked --profile ci --no-tests=pass

test-doc:
    cargo test --workspace --doc --all-features --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

deny:
    cargo deny check

miri:
    cargo +nightly-2026-08-27 miri test --locked -p rustjsi-backend -p rustjsi-core -p rustjsi-runtime -p rustjsi-host -p rustjsi-embed -p rustjsi-module -p rustjsi-ir -p rustjsi-codegen -p rustjsi-testkit

spell:
    typos

links:
    lychee --offline --config lychee.toml .

ci: fmt-check check clippy test test-doc doc

verify: ci deny spell links
