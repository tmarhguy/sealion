# ADR 005 — Postings codec: delta + varint baseline

**Status:** accepted (milestone 05)
**Date:** 2026-10-01

## Context

Posting lists are sorted DocIDs (and within-doc positions are increasing),
so delta + compact-integer encoding is the textbook baseline (spec §20–21).
The segment format carries a compression-kind flag so better codecs slot in
without format churn.

## Alternatives considered

1. **Delta + unsigned LEB128 varint (chosen)** — simplest correct
   baseline; byte-aligned, no SIMD, easy to verify bit-exactly.
2. **Bit-packed / Frame-of-Reference blocks** — better density and decode
   bandwidth, but block metadata, padding rules, and a second code path
   before we have segment-level numbers. Revisit with profiling data.
3. **SIMD-friendly block formats** — same verdict, stronger: needs
   `unsafe` + fuzz + a measured hot-path justification (§94–95).
4. **Raw u64 storage** — kept as a segment-layout flag purely as the
   benchmark baseline, never for production writes.

## Decision

Delta + varint for DocIDs and positions, with strict decoding (truncation,
overflow, and non-increasing data are all `None`/corrupt, never silent).
Block CRCs cover encoded bytes.

## Measured results (2026-10-01, Apple silicon, debug profile)

Codec micro-benchmarks (`cargo test -p sealion-index --test
compression_bench -- --nocapture`):

| shape | n | raw bytes | compressed | B/posting | decode |
|---|---|---|---|---|---|
| dense 1..100k | 100,000 | 800,008 | 100,003 | 1.00 | ~6.5 MB/s, ~6.5M post/s |
| sparse uniform /5M | 19,956 | 159,656 | 31,997 | 1.60 | ~9.8 MB/s, ~6.1M post/s |
| clustered runs | 61,654 | 493,240 | 62,325 | 1.01 | ~7.4 MB/s, ~7.3M post/s |

Segment level, 2000 synthetic docs / 102,668 postings
(`sealion-query/tests/compression_segments.rs`):

| layout | segment bytes |
|---|---|
| raw u64 | 4,110,718 |
| delta+varint | 1,923,449 (46.8% of raw) |

Search results are identical across layouts on rare/common/title/AND
query mixes. The 46.8% figure includes stored JSON documents and the
dictionary, so it *understates* postings-only compression — postings-only
ratios are the 1.0–1.6 B/posting above versus 8 B raw.

Debug-profile numbers are a floor, not a claim: release LTO, a
criterion-style harness (milestone 16/20), and 100K+ corpora (§85) come
later. No SIMD work until profiling shows decode on the hot path.

## Tradeoffs

- Varint decode is a byte-at-a-time loop (~6–10 MB/s here); bit-packing
  should beat it several-fold on dense lists. That is a measured future
  optimization, not today's guess.
- Positions dominate small-posting storage (one varint per occurrence);
  position-block sharing/dedup is unexplored.

## Consequences

- Production writes always set both compression flags; raw mode exists
  only for benchmarks.
- Any new codec must reproduce these tables (better numbers, identical
  search results) before replacing the baseline.
