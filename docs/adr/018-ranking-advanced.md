# ADR 018: Additive Blends, Measured Authority, Deferred Vectors

Recorded after milestone 18 implementation. Applies to `rank::score_doc`,
`sealion index authority`, and ranking configuration.

## Context

Spec §54–59 wants link authority, freshness, ablations, and *optionally*
hybrid vector retrieval — with explicit brakes: don't assume popularity
helps (§55), freshness is query-dependent (§56), vectors only if they
genuinely improve (§57).

## Decision

- **Additive blends**: `score = lexical + w_auth·authority + w_fresh·recency`.
  Authority is PageRank normalized to [0,1] over in-corpus outlinks
  (published as a shadowing generation via `index authority`, so no
  format change and cluster-safe per-shard writes from the global graph).
  Freshness is exponential decay against the corpus-newest timestamp
  (computed locally or globally, so distributed scoring stays exact).
  Both default to 0 (lexical-only until eval justifies).
- **WAND stays exact** with blends on: the corpus-wide max addition folds
  into every term/block bound (over-bound = less skipping, never wrong),
  locked by blend-enabled equivalence tests.
- **TF-IDF scorer** ships (§27 debt): same IDF, `(1+ln(tf))` saturation.
  Identical order to BM25 on the seed set — reported, not hidden.
- **Hybrid vectors deferred**: no embedding runtime, no judgment scale to
  measure semantic recall, lexical headroom remains. The blend slots and
  the eval harness are the future plug-in points.

## Consequences

- `Explain` carries `authority`/`freshness`; `--explain` shows them.
- Ranking PRs now have three mechanical gates: oracle equivalence,
  WAND equivalence (incl. blends), relevance regression.
