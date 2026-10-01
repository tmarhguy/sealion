# Correctness

Milestone 17. What SeaLion guarantees, which oracle guards each property,
and where the tests live. Rule: every search-algorithm change is verified
against the reference engine (§26, §108, §117); every ranking change
additionally passes the relevance gate (§53).

## 1. Properties (§96)

| Property | Oracle / check | Location |
|---|---|---|
| `decode(encode(postings)) == postings` | codec round-trip + garbage self-consistency | `sealion-index` unit + `tests/adversarial.rs` |
| `merge(segments)` preserves results | live-set equality vs clean `MemIndex` | `tests/generations.rs`, `tests/properties.rs` |
| `incremental == clean rebuild` | random ADD/UPDATE/DELETE sequences vs mirrored `MemIndex` (membership, postings, tombstones) | `tests/properties.rs` |
| `optimized top-k == exhaustive top-k` | WAND + Block-Max WAND vs `ranked_search` on hand + fuzz corpora, segments, generations | `sealion-query` wand tests, `tests/wand_segments.rs` |
| `cluster top-k == single-index top-k` | coordinator vs single segment, all query shapes | `sealion-distributed/tests/cluster_equivalence.rs` |
| phrase hits truly contain the phrase | oracle-independent adjacency invariant on randomized docs | `tests/phrase_rank.rs` |
| `deleted docs never appear` | tombstone + shadowing vs clean rebuild; reference engine | `tests/properties.rs`, `tests/generations.rs` |
| BK-tree == naive scan | agreement on generated vocabularies | `spell` unit tests |
| galloping == linear | skewed/even/empty/disjoint cases | `execute` unit tests |
| index search == reference oracle | deterministic + LCG fuzz over Boolean queries | `tests/boolean_reference.rs`, `reference` tests |

## 2. Fuzzing posture (§98)

In-repo deterministic fuzz (LCG, fixed seeds): query parser (2000
garbage inputs — total, bounded leaves), Levenshtein metric laws,
BK-tree agreement, delta-codec garbage, garbage/truncated segments,
per-region single-byte mutations, garbage manifests, garbage HTML.
Nightly `cargo-fuzz` harnesses (libFuzzer) for the parser, segment
reader, and HTML extractor are the documented next step, not yet wired.

## 3. Relevance gate (§53, §108)

`sealion eval relevance [--save/--baseline]` — nDCG@10 primary. Baseline
on the seed set: nDCG@10 = 0.9314 (recorded milestone 12).

## 4. Definition-of-done checklist (§108, per feature)

Implementation + unit tests + integration tests + docs + metrics, plus:
search changes → oracle comparison; ranking changes → relevance report;
perf changes → before/after bench; format changes → crash/corruption
tests. CI enforces `fmt --check`, `clippy -D warnings`, full `cargo test`.
