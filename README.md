<h1 align="center">SeaLion</h1>
<p align="center"><strong>A distributed full-text search engine built from first principles in Rust.</strong></p>
<p align="center">
  <a href="https://github.com/tmarhguy/sealion-search-engine/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/tmarhguy/sealion-search-engine/actions/workflows/ci.yml/badge.svg"></a>
  <a href="docs/architecture.md"><img alt="Status: active development" src="https://img.shields.io/badge/status-active%20development-2ea043"></a>
  <a href="https://github.com/tmarhguy/sealion-search-engine/actions/workflows/ci.yml"><img alt="Tests: 155 passing" src="https://img.shields.io/badge/tests-155%20passing-2ea043"></a>
  <a href="https://www.rust-lang.org/"><img alt="Rust" src="https://img.shields.io/badge/engine-Rust-b7410e"></a>
</p>

<p align="center">
  <img alt="SeaLion demo — search page with BM25-ranked hits (2x speed)" src="media/demo/sealion-demo.gif" width="560">
  <br>
  <em>Demo: the search page — type a query, get BM25-ranked hits with snippets (2x speed).</em>
</p>

<p align="center">
  <a href="https://sealion.tmarhguy.com"><img alt="Try Live Search" src="https://img.shields.io/badge/Try_Live_Search-sealion.tmarhguy.com-1d4e89?style=for-the-badge"></a>
  <a href="https://tmarhguy.github.io/sealion/"><img alt="Read the Docs" src="https://img.shields.io/badge/Read_the_Docs-tmarhguy.github.io-2e7d32?style=for-the-badge"></a>
</p>

SeaLion is a distributed full-text search engine built from first
principles in Rust. Analysis, inverted index, segments, compression,
and ranking are implemented directly in this repo.

**What it is**

| | |
|---|---|
| **Analyzes** | text into immutable versioned segments |
| **Ranks** | field-aware BM25 |
| **Skips** | exact Block-Max WAND |
| **Tolerates** | typos, BK-tree correction |
| **Crawls** | politely, HTML/image-alt extraction |
| **Measures** | relevance, nDCG@10 = 0.9314 seed |
| **Scales** | shards and replicas with failover |
| **Serves** | JSON + search page over HTTP (`sealion serve`) |
| **Measured** | 5114 docs/s indexing, 271 qps exact WAND search (release) |

## Quick start

```bash
cargo build --workspace
./target/debug/sealion index ./evaluation/corpus --data-dir ./sealion-data
./target/debug/sealion search "distributed systems" --data-dir ./sealion-data
./target/debug/sealion serve --bind 127.0.0.1:8080 --data-dir ./sealion-data
```

Full CLI: `sealion --help`. Same checks as CI:
`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`.

## Docs

- [Technical manual](https://tmarhguy.github.io/sealion/) — the whole
  system, built from [`docs/index.adoc`](docs/index.adoc) with `make docs`
- [architecture](docs/architecture.md) — design and build order
- [segment format](docs/index-format.md) — the `.seal` byte layout
- [ADRs](docs/adr/) — 001–019, every important decision
- [correctness](docs/correctness.md) — test gates and invariants
- [performance](docs/performance.md) — measured numbers only

### Local documentation and reusable theme

```bash
make docs-check
python3 -m http.server 8000 --bind 127.0.0.1 --directory build/docs
```

Open [localhost:8000](http://localhost:8000). The notebook theme uses plain CSS
and JavaScript with the existing Asciidoctor build—no Node dependencies.
See the [theme guide](docs/theme/README.md) and [starter document](docs/theme/starter.adoc)
to reuse it in another project.

## Author

<p align="center">
  <a href="https://tmarhguy.com"><img alt="Website" src="https://img.shields.io/badge/Website-tmarhguy.com-1d4e89?style=for-the-badge"></a>
  <a href="https://github.com/tmarhguy"><img alt="GitHub" src="https://img.shields.io/badge/GitHub-tmarhguy-24292f?style=for-the-badge&logo=github"></a>
</p>
