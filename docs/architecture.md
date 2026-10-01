# SeaLion Architecture

Source of truth for the system design; the full requirements live in the
project spec (§0–119). This document tracks what exists today and what each
milestone adds.

## Current state

**Milestones 01–03 — bootstrap, analysis, in-memory search.**

- 01: Rust workspace, `sealion` CLI skeleton with stubbed subcommands,
  document model (`sealion-core`), validated TOML config, CI
  (fmt/clippy/test). Plus bootstrap correctness fixes: quoted TOML strings
  accepted for `stemmer`, unimplemented `sealion-eval` modules stubbed so
  the workspace builds.
- 02: text analysis pipeline (`sealion-index::analysis`, `::stemmer`):
  Unicode-alphanumeric tokenizer, case normalization, English stop words,
  hand-implemented Porter stemmer (validated against Porter's published
  vocabulary), per-field config overrides, gapped positional terms.
  See `docs/adr/002-analysis.md`.
- 03: SeaLion searches. In-memory fielded positional inverted index
  (`sealion-index::mem_index`), Boolean query AST over normalized terms
  with raw-text constructors (`sealion-query::query`), indexed execution
  (`sealion-query::execute`), and the mandatory reference oracle
  (`sealion-query::reference`) with index/reference agreement tests
  (unit + integration + deterministic fuzz). See
  `docs/adr/003-inmemory-index-reference.md`.

Not yet: persistent segments (04), compression (05), incremental
updates/merging (06), ranking (07), query parser/planner (08), WAND (09),
typos/autocomplete (10), crawler (11), eval metrics (12), distribution
(13–15), product (19–20). The CLI `search`/`index` subcommands stay
stubbed until persistent indexes land.

## Local build note (macOS)

Some machines have a CommandLineTools/Xcode SDK too new for the installed
linker (`arm64e.x1-macos` errors in `libSystem.tbd`). If `cargo build`
fails at link time, build with an older SDK, e.g.:

```bash
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk
cargo build --workspace
```

This is environment-specific; CI (ubuntu-latest) is unaffected.

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
