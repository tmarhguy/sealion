# ADR 013: Hash Sharding With Global-Stats Coordination

Recorded after milestone 13 implementation. Applies to
`sealion-distributed::{shard,coordinator}`.

## Context

Spec §60–65 requires document sharding, deterministic assignment,
parallel fan-out, and globally consistent BM25 (§63) with correct global
merging (§64–65).

## Decision

- Assignment `fnv1a64(doc_id) % N` (stable, no lookup, shard count fixed
  at init).
- Exactness via global stats: gather (N, field totals, per-term df) and
  a union vocabulary first; every shard scores its Boolean candidates
  under identical statistics through the new
  `rank::ranked_search_with_stats` / `CollectionStats` API (also reused
  by single-node search with local stats — one code path, no drift).
- Parallelism via tokio `spawn_blocking` over in-RAM readers; merge is a
  plain comparable-score sort.

## Consequences

- `cluster_matches_single_index` locks exactness; any scoring change
  must keep it green alongside the §41 WAND tests.
- Shard count changes need reindexing (documented alternative: ranges).
