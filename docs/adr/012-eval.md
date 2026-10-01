# ADR 012: Relevance Harness With Basename Judgments

Recorded after milestone 12 implementation. Applies to `sealion-eval`
and `sealion eval relevance`.

## Context

Spec §51–53 requires labeled judgments, nDCG/MRR/MAP, and a regression
gate. Judgments must outlive any single index build (stable across
machines and generations).

## Decision

- TSV formats with `id\ttext` queries and `query\tgrade\tbasename`
  judgments; basenames resolve to DocIds at eval time (machine-independent,
  generation-independent).
- Binary metrics use grade ≥ 1; nDCG uses exponential gains; ideal from
  all judgments (misses hurt).
- Harness self-indexes the eval corpus into a temp dir and scores with
  exhaustive BM25; reports save/compare as JSON for the regression gate.
- Seed set is deliberately small (validates the harness, catches
  regressions); TF-IDF/authority/hybrid ablations wait for milestones
  18/20 with more judgments.

## Consequences

- `sealion eval relevance` runs out of the box; baseline nDCG@10 = 0.9314
  recorded 2026-10-01.
- Every ranking PR now has a mechanical gate instead of demo queries.
