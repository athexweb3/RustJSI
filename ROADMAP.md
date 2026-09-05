# Roadmap

RustJSI is in feasibility work. Development starts with the highest-risk
runtime and engine assumptions.

## Planned sequence

1. **Evidence and feasibility** — repeatable benchmarks, allocation/copy
   accounting, and lifecycle experiments.
2. **Safe semantic core** — scoped values, runtime affinity, errors, and a
   deterministic test model.
3. **First real embedding path** — one JavaScript engine with calls, errors,
   teardown, and a minimal native object.
4. **Long-lived behavior** — roots, buffers, callbacks, asynchronous work,
   cancellation, and resources.
5. **Module tooling** — a portable module description and generated bindings.
6. **Portability and operations** — a second backend, compatibility testing,
   observability, security review, and stress testing.
7. **External adoption** — experimental releases, pilots, stabilization, and
   only then a narrowly supported production line.

## Current scope

The current work is validating the source-linked backend boundary and host
lifecycle model against both a deterministic implementation and direct
JavaScriptCore evidence. The common JSC path currently covers scoped values,
strict scalar reads, value classification, explicit roots, and Rust-owned
external buffers during a standalone host-authorized entry. It is not ready to
publish crates or claim runtime compatibility, general performance, or
production readiness.

The shared host entry gate now provides bounded admission and deferred
invalidation accounting, exercised against the lifecycle model and both JSC
entry paths. An experimental source-linked `Host` contract confines backend
access to a host-authorized closure, including foreign-owned JSC contexts. A
complete host architecture still needs attached-runtime leases, active
runtime/epoch validation, scheduling, and synchronization evidence from a real
framework integration.

## Deferred work

The project does not currently include a React Native module framework,
cross-engine parity, a public binary plugin ABI, a TypeScript-first frontend,
or broad platform bindings. Those areas require separate implementation and
evidence before becoming public commitments.
