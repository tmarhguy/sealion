# ADR 014: File-Copy Replication With Failover Routing

Recorded after milestone 14 implementation. Applies to
`sealion-distributed::replica` and the coordinator's open path.

## Context

Spec §66–69: optional replicas per shard, routing strategies with
benchmarks, health tracking, no incompatible generations, explicit
partial semantics. And no consensus reimplementation (§66).

## Decision

- Replicas are verified file copies (segments + manifest, checksums on
  open). No consensus: immutable files make copy-verify sufficient.
- Router: round-robin default, least-outstanding optional; open failure
  fails over to the next healthy copy transparently; total loss →
  `partial: true` + diagnostics, never silent.
- Health states + generations + heartbeats in `cluster.json`; stale
  copies skipped.

## Consequences

- Kill-a-copy demo works: traffic shifts, results complete; kill-all
  serves survivors as partial. Both are tested and CLI-verified.
