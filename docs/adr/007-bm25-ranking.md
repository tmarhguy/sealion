# ADR 007: BM25 Field-Aware Ranking (Exhaustive Baseline)

Recorded after milestone 07 implementation. Applies to `sealion-query::rank`.

## Context

Search was unranked Boolean (DocId order). Spec §27–29 requires TF-IDF
baseline then BM25 as the primary lexical ranker with field weights, plus
phrases/proximity features, snippets (§42), and `--explain` (§50). WAND
(milestone 09) needs an exact baseline to verify against (§41).

## Decision

- **Formula**: `IDF = ln(1 + (N − df + 0.5)/(df + 0.5))` (always positive);
  `BM25_f = IDF·(tf·(k1+1))/(tf + k1·(1−b+b·len/avgdl))`;
  `score = Σ_fields weight_f · Σ_terms BM25_f`. Knobs from `RankingConfig`
  (`k1=1.2, b=0.75`, title 4.0 / heading 2.0 / body 1.0 / anchor 1.5).
- **Stats**: `N` = live docs; `df` = live postings length (shadow-aware);
  lengths/avgs via `IndexView::field_token_total/field_length`
  (stored totals for `MemIndex`/`SegmentReader`, adjusted sums for
  `MultiSegmentView`, scan fallback). Correct over generations.
- **Phrases** (`Query::Phrase`): doc-set intersection + consecutive-position
  check; scored as sum of constituent BM25 (no bonus yet — tuned with eval
  data in milestone 18). Quoted CLI queries `"a b"` search body phrases.
- **Exhaustive top-k**: Boolean candidates → score all → sort
  (score desc, DocId asc) → truncate. This is the §41 baseline for WAND.
- **Snippets**: word-based, stemming-aware highlight (whole words whose
  analyzed form equals a query term), `<mark>` + HTML-escaped, ~180 chars.
- **CLI**: `search` is BM25-ranked with snippets; `--explain` prints
  per-term (field, tf, df, idf, len, avg, contrib) breakdowns.

## Alternatives

- Classic Robertson/Sparck-Jones IDF `ln((N−df+0.5)/(df+0.5))` (can go
  negative): rejected — negative contributions confuse explain and merge
  tiebreaks; Lucene-style positive variant is standard practice.
- Per-block max-score caching now: deferred to milestone 09 (Block-Max).

## Consequences

- `TopK_exhaustive` defined; WAND must match it exactly.
- Known cost: stats/lengths re-analyze stored docs on multi-segment views
  (correct, not yet cached — profiling/caching in milestone 16).
