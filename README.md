# SeaLion

**A distributed full-text search engine built from first principles.**

SeaLion crawls, indexes, ranks, serves, evaluates, and explains search results
across large document collections — without Lucene, Elasticsearch, OpenSearch,
Solr, Meilisearch, Typesense, or any other search engine at its core.
Every inverted-index structure and retrieval algorithm is implemented here.

## Status

**Under construction.** Milestones 01–03 of 20 are complete: the engine
builds, analyzes text, and runs Boolean search over an in-memory index
(verified against the reference oracle). Persistent segments, ranking, and
the CLI search path are roadmap — unbuilt features are marked honestly as
*not yet implemented*, numbers that haven't been measured are *TBD
(unmeasured)*.

## Quick start (as milestones land)

```bash
# Build
cargo build --release

# Index a local corpus          (milestone 02+, not yet implemented)
sealion index ./corpus

# Crawl the web                 (milestone 11, not yet implemented)
sealion crawl https://example.org

# Search                        (milestone 03, not yet implemented)
sealion search "distributed systems"
sealion search '"distributed systems"' --explain
```

## Layout

| Path | Contents |
|---|---|
| `crates/sealion-core` | Document model, fields, config, errors |
| `crates/sealion-index` | Segments, postings, dictionary, compression, merging |
| `crates/sealion-query` | Parser, planner, execution, WAND, ranking, snippets, spelling |
| `crates/sealion-crawler` | Frontier, fetch, politeness, dedup |
| `crates/sealion-distributed` | Coordinator, shards, replication, routing |
| `crates/sealion-eval` | Relevance judgments, nDCG/MRR/MAP, regression |
| `crates/sealion-api` | HTTP search + admin API |
| `crates/sealion-bench` | Benchmark harnesses |
| `crates/sealion-cli` | `sealion` command-line interface |
| `docs/` | Architecture, format specs, ADRs |

## Principles

1. **Correctness first.** Every search optimization is verified against an
   intentionally-slow reference engine (§26). Optimized top-k must equal
   exhaustive top-k (§41) unless a strategy is documented as approximate.
2. **Measured, not intuited.** No optimization lands without a profile and a
   before/after benchmark; no ranking change lands without a relevance
   regression (§53); no number is published without being measured.
3. **Documented decisions.** Important choices get Architecture Decision
   Records in `docs/adr/`.
4. **Honest status.** Roadmap items stay labeled roadmap; unverified claims
   stay out of docs and resumes.

## Docs

- `docs/architecture.md` — system design and build plan
- `docs/adr/` — architecture decision records

## License

TBD — to be chosen by the project owner before any public release.
