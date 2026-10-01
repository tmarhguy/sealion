# ADR 002 — Text analysis: tokenizer, normalization, stop words, stemming, positions

**Status:** accepted (milestone 02)
**Date:** 2026-10-01

## Context

Indexing and query parsing must share one normalization pipeline (spec §12,
§31) so query terms match indexed terms exactly. The pipeline is
config-driven: global `[analysis]` knobs with per-field overrides
(`[analysis.<field>]`), already modeled in `sealion-core::config`.

## Alternatives considered

1. **Hand-written tokenizer + Porter + stop list (chosen)** — maximal
   control, zero new dependencies, keeps every IR structure ours (§2).
2. **`regex` crate for tokenization** — heavier than needed for
   alphanumeric runs; adds a dependency to the hot path for no IR benefit.
3. **External stemmer (`rust-stemmers`)** — stemming *is* core IR research
   here, not infra; outsourcing it weakens the from-first-principles claim
   and complicates query/index compatibility auditing.
4. **Tantivy or similar for analysis pieces** — ruled out by §2 core
   restriction.

## Decision

- **Tokenizer:** maximal runs of Unicode alphanumeric characters
  (`char::is_alphanumeric`). Punctuation, whitespace, symbols, and `_` are
  separators. No case preservation: lowercase immediately
  (`str::to_lowercase`, Unicode-aware).
- **Stop words:** fixed English list, sorted slice + binary search, applied
  on the lowercased pre-stem form. Toggleable globally and per field.
- **Stemming:** Porter (1980) implemented from the paper in safe Rust,
  validated against Porter's published test vocabulary (in-module tests).
  ASCII-only; non-ASCII terms pass through unchanged. `none` disables it.
- **Positions:** raw-token numbering **with gaps** for filtered tokens
  (`"the distributed database"` → `distributed@1 database@2`). Dense
  numbering would corrupt phrase adjacency across stop words; phrase search
  (milestone 07) depends on these gaps.
- **Length filter:** `min_token_len` applies after stemming, per field.

## Tradeoffs

- Unicode normalization is lowercase-only; no NFKC folding or decompounding
  yet. Documented limitation, revisit with non-English corpora.
- The stop list is a fixed general-English set, not corpus-derived. Corpus
  statistics (milestone 07+) may inform per-field stop policy later.

## Consequences

- `sealion-index::analysis::Analyzer` is the single entry point for both
  indexing and query normalization; `Query::term_raw` (milestone 03) builds
  on it, making §31 agreement structural rather than conventional.
- Any pipeline change must re-verify index/query agreement via the reference
  engine (§26) and the integration tests.
