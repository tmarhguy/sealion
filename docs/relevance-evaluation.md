# Relevance Evaluation

Milestone 12. Grades, metrics, harness, and the regression gate every
ranking change must pass (§51–53, §108).

## 1. Judgments (§51)

- Grades: `0 = irrelevant, 1 = marginal, 2 = relevant, 3 = highly relevant`.
- Files live under `evaluation/` (separate from any production index):
  `corpus/` (documents), `queries/queries.tsv` (`<id>\t<text>`),
  `judgments/judgments.tsv` (`<query-id>\t<grade>\t<doc basename>`).
- Basenames (not full URLs) keep judgments machine-independent; the
  harness resolves them to live DocIds through the index under test.
- Seed set: 8 documents, 4 queries, 9 judgments — enough to validate the
  harness and catch regressions, not a publishable benchmark. Growing it
  (more queries, pooled unjudged docs, graded head/torso/tail split) is
  milestone 20 work.

## 2. Metrics (§52)

| Metric | Relevance rule | Notes |
|---|---|---|
| nDCG@10 (**primary**) | exponential gains (3→7, 2→3, 1→1) | ideal from *all* judgments, so misses hurt |
| MRR | grade ≥ 1 | first relevant rank |
| MAP | grade ≥ 1 | mean of average precisions |
| P@10 / R@10 | grade ≥ 1 | recall against all judged relevant |

nDCG implementation detail worth knowing: the ideal ranking is built
from every judged grade for the query, not just retrieved docs (an early
version normalized by retrieved-only ideal and scored misses as 1.0 —
caught by unit test, fixed).

## 3. Harness

`sealion eval relevance [--eval-dir DIR] [--save report.json]
[--baseline old.json]`:

1. indexes the eval corpus into an isolated temp segment (never touches
   the user index);
2. resolves basenames → DocIds;
3. scores every query with exhaustive BM25 (the §41 baseline);
4. prints means + per-query rows;
5. optionally saves the JSON report / compares against a baseline.

Baseline run (2026-10-01, seed set): **nDCG@10 = 0.9314, MRR = 1.0000,
MAP = 0.7917**. Multi-term AND semantics explain the misses (e.g. q2
retrieves only docs containing *all* terms in one field).

## 4. Regression gate (§53)

Every ranking change must print old-vs-new: ΔnDCG@10, ΔMRR, ΔMAP,
queries improved/regressed/unchanged, and the largest per-query
regressions. A change does not land because one demo query improved —
nDCG@10 must not regress without a documented reason.

## 5. Ablations (§93, milestone 18)

Run with config variants (`--config`); compared via `--save`/`--baseline`.

| Row | Config | nDCG@10 | Δ vs BM25 |
|---|---|---|---|
| TF-IDF | `scoring = "tfidf"` | 0.9314 | +0.0000 (identical order on the seed set) |
| BM25 | default | 0.9314 | — |
| BM25 + authority | `authority_weight = 5` after `index authority` | *corpus-dependent* | reorder shown on the link-mesh fixture below |
| BM25 + freshness | `freshness_weight = 5` | *corpus-dependent* | 2020 doc demoted to last on the mesh |

Seed-set note: TF-IDF and BM25 rank identically on 8 tiny documents —
the ablation is honest, not impressive; the formulas differ (verified by
unit test) but the corpus is too small to separate them. Authority and
freshness cannot move the seed set at all (no outlinks, uniform mtimes),
so they were measured on a 5-page HTML mesh instead:

- Authority: `opt.html` (PageRank 0.52) rises above `solo.txt` (0.08)
  despite weaker lexical score (0.29 vs 0.32) — blend reorders as designed.
- Freshness: the 2020 page scores recency 0.0000 (82 half-lives) and drops
  last; fresh pages gain +5.0.

Defaults stay `authority_weight = 0`, `freshness_weight = 0`: blends are
opt-in until a larger judgment set justifies values (§55: do not assume
popularity helps — evaluate it).

## 6. Hybrid retrieval (§57–59): evaluated, deferred

Vector/ANN search (HNSW + hybrid BM25+semantic) was evaluated against the
spec's own gate — *"only after lexical SeaLion is excellent"* and *"must
not replace the search engine"*. Verdict for v1.0: **deferred**. Reasons:
no embedding runtime in the dependency closure, no judgment set large
enough to measure semantic recall honestly, and the lexical stack
(BM25 + fields + proximity + authority + freshness) has headroom left.
The harness (`evaluate`/`compare_reports`) and the blend architecture
(additive, weighted, explainable) are exactly what a future hybrid row
would plug into. See ADR-018.
