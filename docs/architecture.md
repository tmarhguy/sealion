# SeaLion Architecture

Source of truth for the system design; the full requirements live in the
project spec (§0–119). This document tracks what exists today and what each
milestone adds.

## Current state

**Milestone 01 — bootstrap.** Rust workspace, `sealion` CLI skeleton with
stubbed subcommands, document model (`sealion-core`), validated TOML config,
CI (fmt/clippy/test). No indexing or search yet.

## Design overview

### Search path

```text
USER → frontend → API → query coordinator
    → parse/normalize → rewrite/correct → query plan
    → parallel shard fan-out → local top-k per shard
    → global merge → final ranking → snippets → USER
```

### Indexing path

```text
seeds/files → crawl frontier → fetch workers → document parser
    → normalization → tokenizer/analyzer → indexer
    → segment builder → immutable segment → publish → background merge
```

## Crate map

| Crate | Owns | Milestones |
|---|---|---|
| `sealion-core` | `Document` model (§5), field taxonomy (§15), config (§104), errors | 01 |
| `sealion-index` | analysis (§12–13), in-memory index (§14), segments (§17–19), dictionary (§16), compression (§20–22), merging (§23), updates (§24–25) | 02–06 |
| `sealion-query` | reference engine (§26), TF-IDF/BM25 (§27–29), parser (§30–32), Boolean (§33), phrases/proximity (§34–35), top-k: exhaustive/WAND/Block-Max WAND/MaxScore (§37–40), snippets (§42), spelling/fuzzy/autocomplete (§43–47), rewriter (§49), explain (§50) | 03, 07–10 |
| `sealion-crawler` | frontier (§7), politeness (§8), canonicalization (§9), dedup (§10), HTML (§11) | 11 |
| `sealion-eval` | judgments (§51), metrics (§52), regression (§53) | 12 |
| `sealion-distributed` | sharding (§60–61), coordinator (§62–65), replicas (§66–69), indexing (§70), rebalancing (§71) | 13–15 |
| `sealion-bench` | corpora/queries (§85–86), indexing/query/compression/WAND/distributed/failure/relevance benchmarks (§87–93) | 16, 20 |
| `sealion-api` | search + admin HTTP API (§76–77), traces (§82), metrics (§83) | 19–20 |
| `sealion-cli` | all `sealion` subcommands (§103) | 01, then per milestone |

## Key design decisions (ADRs)

- `docs/adr/001-core-language.md` — Rust for the engine.

Future ADRs (one per important decision): segment format, postings codec,
term dictionary, BM25 statistics distribution, top-k execution strategy,
sharding, replication, autocomplete structure, hybrid ranking.

## Build order (§110–111)

```text
documents → tokens → in-memory index → disk index → compression
→ incremental indexing → BM25 → query language → WAND
→ typos/autocomplete → crawler → relevance science
→ distributed search → replicas → rebalancing → profiling
→ adversarial testing → advanced ranking → product
```

## Non-negotiables

- §2: no Lucene/ES/OpenSearch/Solr/Meilisearch/Typesense/Algolia-as-engine/Postgres-FTS-as-engine. Infra libs OK.
- §26: reference engine is mandatory; every search-algorithm change verified against it.
- §41: optimized top-k == exhaustive top-k unless explicitly documented approximate.
- §108: feature = implementation + unit tests + integration tests + docs + metrics.
- Honest numbers only: unmeasured values are "TBD (unmeasured)".
