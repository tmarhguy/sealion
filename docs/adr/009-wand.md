# ADR 009: WAND and Block-Max WAND Top-k Retrieval

Recorded after milestone 09 implementation. Applies to
`sealion-query::wand`.

## Context

Exhaustive scoring (milestone 07) scores every Boolean candidate. Spec
§38–39 requires WAND candidate skipping and Block-Max WAND, with §41
exactness: optimized top-k must equal exhaustive top-k.

## Decision

- **Shared math**: `rank::bm25_term_contrib` is the single BM25
  implementation; exhaustive and WAND call it identically (same term order
  → identical floats).
- **WAND**: textbook pivot loop over per-term posting cursors with exact
  per-term max contributions as upper bounds. Boolean membership
  (AND/NOT) is gated by binary search in the candidate set *before* heap
  insert, so non-members can never poison the threshold.
- **Block-Max WAND**: postings windowed into 64-entry blocks with per-block
  maxima (computed per query in memory). A list's whole block is jumped
  only when `block_max + Σ other uppers < threshold − EPS` — sound for
  every doc in the block. Persisting block maxima in the segment format is
  deferred to milestone 16 (caches/profiling).
- **Tie/EPS rule**: all bound-vs-threshold comparisons use `EPS = 1e-6`
  slack and `>`/`<` strictness so exact ties always evaluate; final order
  is always (score desc, DocId asc). This is what makes §41 hold under
  float arithmetic, not just in theory.
- **Fallbacks**: queries with no scoring terms (site-only) or `top_k == 0`
  delegate to exhaustive paths with identical output.
- **CLI**: search uses Block-Max WAND by default; `--exhaustive` selects
  the baseline; `--explain` prints `scored=/skipped=/blocks_skipped=`
  (the §90 ablation inputs) and both paths print identical scores.

## Alternatives

- Approximate top-k (delta-trimmed heaps): rejected — §41 requires exact.
- MaxScore instead of WAND: not implemented yet; WAND covers the flagship
  requirement and MaxScore remains a documented optional comparison.

## Consequences

- `wand_matches_exhaustive_*` tests (generated fuzz over Boolean shapes
  and top-k values, segment-backed, multi-generation with tombstones)
  lock §41. Any future scoring change must keep them green.
- Upper bounds and block maxima are recomputed per query (O(postings)
  precomputation). Fine for correctness baseline; profiled caching is
  milestone 16 work.
