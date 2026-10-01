# ADR 017: Deterministic Adversarial Suites, Fuzz Later

Recorded after milestone 17 implementation. Applies to workspace tests,
`docs/correctness.md`, `docs/failure-model.md`.

## Context

Spec §96–101 demands properties, fuzzing, corruption, failure injection,
and soak testing. Full libFuzzer integration and nightly soak infra are
heavy; the highest-value coverage (oracle agreement, crash atomicity,
corruption detection) can be deterministic and CI-fast.

## Decision

- Deterministic LCG suites in-repo for properties (random op sequences
  vs clean rebuild), parser/codec/segment/manifest/HTML garbage, and
  per-region corruption — all in `cargo test`, all green.
- Failure injection via filesystem states (missing dirs, corrupt bytes,
  file-as-dir) instead of fault-injection frameworks — root-proof and
  hermetic.
- Soak as an `#[ignore]`d test with env-tunable iterations; nightly
  `cargo fuzz` harnesses documented as next, not wired.

## Consequences

- CI stays fast and hermetic; adversarial coverage grows with the code.
- The failure model doc, not tribal knowledge, defines expected behavior.
