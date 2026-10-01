# ADR 016: Profiled Optimizations and Justified Caches

Recorded after milestone 16 implementation. Applies to scoring paths,
`SegmentReader` memos, `sealion-query::cache`, and `sealion bench`.

## Context

Spec §94 requires profile-first optimization; §72 requires every cache
to justify itself; §85–93 require benchmark harnesses with measured
numbers. An early decode cache measured 1.01× — kept honest by this ADR.

## Decision

- Fetch-once postings + per-query length memo (44×, §1 of
  `docs/performance.md`); ingestion fix-up scoping (2.4×); reader length
  memo + WAND non-member skip (WAND 1.27× over exhaustive, exact).
- Keep: decode memo (marginal but free, cross-query reuse, counters),
  length memos (dominant cost), query-result LRU (4× at 0.67 hit rate,
  generation-gated per §73) for harness/server use.
- Deliberately not cached: CLI one-shot searches and the coordinator
  (nothing to reuse per invocation); second dictionary/block structures
  (segment is already memory-resident); persisted block-max (format
  change deferred until profiling demands it).
- Ship `sealion bench query` (mix, ablation `--exhaustive`, `--cache`
  justification) and `sealion bench index` (docs/s, MB/s, size ratio).

## Consequences

- `docs/performance.md` is the running record; new optimizations append
  case studies with before/after numbers or they don't land.
- Bench commands double as the milestone 20 suite foundation.
