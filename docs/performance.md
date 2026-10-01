# Performance Engineering

Milestone 16. Every optimization here followed §94 (profile → hypothesize
→ implement → benchmark → document) and every cache justifies itself with
numbers (§72). Machine: Apple Silicon macOS, release profile unless noted.

Corpora: 2000-doc synthetic (40-word docs over a 30-word vocabulary,
48k postings, 2.0 MB segment); 8-doc eval set for relevance; 9-query mix
(§86 subset) × 3 rounds for query benches.

## 1. Case study: scoring restructure (44×)

**Profile.** Ranked search over a segment re-fetched and cloned each
term's full posting list *per candidate document* (`view.postings` in the
inner loop), and re-analyzed stored field lengths per (term, doc). A
25-query battery over 2000 docs took **3.5 s** (debug).

**Hypothesis.** Per-query work should be: decode each term once, compute
each (doc, field) length once.

**Change.** `rank::score_doc` now takes prefetched postings (binary
search per candidate) plus a per-query `LengthMemo`; WAND's `full_score`
shares the memo across candidates.

**Result.** Same battery: **80 ms** (debug) — **44×**. All §41 exactness
tests still green (identical floats, same summation order).

## 2. Case study: ingestion fix-up scope (2.4×)

**Profile.** `MemIndex::add_document` re-scanned *every* posting list per
insert to restore DocId order — O(docs × terms).

**Change.** Track touched `(field, term)` keys per insert; fix up only
those lists.

**Result.** 2000-doc build: **445 → 1087 docs/s** (debug), **5114 docs/s,
1.7 MB/s** (release). Index size 3.0× input on tiny docs (stored JSON +
positions dominate; expected to fall on realistic doc sizes — TBD).

## 3. Case study: WAND bound costs (measured, then fixed)

**Profile.** First WAND cut was *slower* than exhaustive (716 ms vs
264 ms mean on the mix, debug): exact upper-bound precomputation
re-analyzed every posting's field length, and OR-space traversal scored
Boolean non-members.

**Changes.** (a) `SegmentReader` memoizes field lengths (first touch
analyzes, rest O(1); safe — lengths are fixed for the build analysis);
`MultiSegmentView` delegates to the owning reader's memo. (b) WAND skips
scoring Boolean non-members after an O(log n) membership check.

**Result (release, 2000 docs).**

| path | qps | mean | p50 | p99 | scored/q | skipped/q |
|---|---|---|---|---|---|---|
| exhaustive | 213 | 4.70 ms | 3.18 ms | 18.35 ms | 937.1 | 0 |
| Block-Max WAND | 271 | 3.69 ms | 2.39 ms | 16.09 ms | 858.9 | 642.3 |

**1.27× qps** with identical top-k (locked by fuzz + segment +
generation equivalence tests). WAND wins modestly here because the corpus
is small and queries broad; selective queries skip far more (unit test:
2 fully scored of 202 with early threshold).

## 4. Caches and their justifications (§72)

| Cache | Mechanism | Measured effect | Verdict |
|---|---|---|---|
| Posting-list decode memo (`SegmentReader`) | decode once, clone per use; CRC at open + first decode | ~1.04× on the mix (decode is already cheap post-restructure) | **kept**: near-zero cost, pays across repeated-term workloads; hit/miss counters exposed via `cache_stats()` |
| Field-length memo (`SegmentReader` + per-query `LengthMemo`) | analyze once per (doc, field) | part of the 44× + WAND recovery above | **kept**: the dominant cost removed |
| Query-result LRU (`sealion-query::cache`, 256 entries, generation-gated) | exact repeat queries skip search | **1090 qps vs 271 (4.0×)** at 0.67 hit rate (1 miss round + 2 hit rounds) | **kept** for harness/server; CLI one-shots and coordinator bypass it (nothing to reuse — documented, not omitted) |

Dictionary/posting-block/document caches (§72 list): the segment file is
fully memory-resident after open, which *is* the block cache; no second
structure added without a profile showing need.

## 5. Known next targets (not yet profiled)

- Persisted block-max bounds in the segment format (would remove WAND's
  per-query bound precomputation entirely).
- Galloping 8× threshold retune (ADR-008 heuristic stands).
- `fuzzy` vocab scan dominates p99 (17–18 ms): BK-tree acceleration.
- Corpus scaling past 2000 docs (100K/1M runs are milestone 20 work).
