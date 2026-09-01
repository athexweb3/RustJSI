## Summary

<!-- What changed, and why. Link the relevant issue or design discussion. -->

- Linked issue/design discussion:

## Invariants and safety

- Affected safety or lifecycle assumptions:
- Unsafe code added or modified? (`unsafe` blocks, FFI, Send/Sync impls):
- Runtime / thread / lifetime impact:
- Allocation / copy / performance impact (measured or explicitly unmeasured):

## Tests

- Tests added or updated:
- Compile-fail / Miri / sanitizer / conformance impact:
- Why existing tests are enough, if none were added:

## Documentation

- README / crate docs / UNSAFE.md updates:

## Compatibility

- Breaking change? (API / ABI / IR / capability / host)
- Generated code affected?
- Version axis affected (core, Host API/ABI, IR, generator, backend, integration):

## Checklist

- [ ] I did not invent a public API or add a dependency without an immediate implemented need.
- [ ] I did not introduce `unsafe` without `// SAFETY:`, `# Safety`, and inventory updates in `UNSAFE.md`.
- [ ] `just fmt-check`, `just clippy`, and `just test` were considered; I report what I actually ran.
- [ ] I did not add placeholder behavior that pretends to work.
- [ ] I did not make an unqualified performance, safety, or production claim.

## Commands actually run

```text
```
