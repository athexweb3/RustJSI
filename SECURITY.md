# Security policy

RustJSI is unpublished experimental software. There is no supported runtime
matrix or response-time commitment yet.

## Reporting a vulnerability

Do not file a public issue for an unresolved vulnerability.

Once private vulnerability reporting is enabled on the project's public GitHub
repository, use that channel. Until a reporting contact is published, do not
assume a monitored security inbox or response SLA exists.

A useful report includes:

1. affected crate, revision, toolchain, OS, and engine/host context;
2. the problem and expected security impact;
3. a minimal reproduction that does not target third-party systems;
4. memory-safety, lifetime, thread, ABI, allocator, or data-exposure impact;
5. any disclosure deadline or embargo constraint.

## Scope

Once implementation exists, in-scope reports include undefined behavior
reachable from safe APIs, invalid runtime/handle lifetime behavior,
cross-thread engine access, boundary unwind failures, buffer aliasing or
allocator errors, diagnostics data exposure, and release-artifact supply-chain
issues.

Out of scope are arbitrary unsafe code in an embedding application, malicious
third-party engine binaries, and permissions explicitly granted by a host to an
otherwise untrusted module.

## Supply chain

The repository uses dependency policy, automated dependency updates, and
workflow review. Future distributed artifacts should include checksums, SBOMs,
signed tags, and provenance attestations.

## Safe harbor

Good-faith research against this project's own public artifacts is welcome.
Do not test against systems you do not own and do not include third-party
exploit payloads in reports or pull requests.
