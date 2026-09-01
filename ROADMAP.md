# Roadmap

RustJSI is at repository-bootstrap stage. Work starts with the highest-risk
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

The current task is to maintain a reliable build, test, documentation, and
security foundation for the first feasibility work. It is not to publish crates
or claim runtime compatibility, performance, or production readiness.

## Deferred work

The project does not currently include a React Native module framework,
cross-engine parity, a public binary plugin ABI, a TypeScript-first frontend,
or broad platform bindings. Those areas require separate implementation and
evidence before becoming public commitments.
