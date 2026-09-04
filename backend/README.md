# `rustjsi-backend`

Capability-oriented engine backend contract for `RustJSI`.

Status: experimental, `0.0.0`, and unpublished.

The crate currently defines a source-linked base scope, versioned capability
manifest, strong-root operations, and an owned external-buffer contract. Scoped
value and byte-view types use generic associated lifetimes; engine-native types
do not enter the portable contract.

`BackendException` owns its message and records whether the backend truncated
it. Constructors preserve that metadata; capture budgets belong to the backend
or host policy, not the metadata container.

This is not a stable ABI or a supported engine interface. The contract will
change as the deterministic model and real backends expose incorrect
assumptions.
