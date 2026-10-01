# ADR 015: Atomic-Flip Shard Movement

Recorded after milestone 15 implementation. Applies to
`sealion-distributed::rebalance` and `sealion cluster move`.

## Context

Spec §71: move shards between nodes without dropping queries, verifying
checksums, with a defined crash story.

## Decision

- Copy → verify-each-by-open → single atomic topology store registering
  the destination (source still present) → best-effort source deletion.
  Crash before store: old routing live. Crash after: harmless orphan.
- Source files are removed only after the flip is durable.

## Consequences

- `moved_shard_preserves_results` locks identical hits across moves;
  orphans are left for operator/GC rather than risking availability.
