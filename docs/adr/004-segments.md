# ADR 004 — Immutable persistent segments

**Status:** accepted (milestone 04)
**Date:** 2026-10-01

## Context

The in-memory index (ADR 003) defines retrieval semantics; SeaLion now
needs them on disk: crash-safe, verifiable, concurrently readable
generations that `sealion index`/`search` can use end-to-end (spec §17–19).

## Alternatives considered

1. **Single self-contained file per segment (chosen)** — header,
   postings/positions blocks, dictionary, stored docs, statistics, footer
   with CRCs in one `.seal` file. One file is trivially copyable (shard
   migration, §71), checksummable, and mmap-ready.
2. **Multi-file segment (dict file + postings file + meta file)** —
   marginally better write concurrency, but publication becomes multi-file
   atomicity (harder crash story) and shard movement copies a set.
3. **SQLite/RocksDB for storage** — rejected: the segment format *is* the
   research artifact (§18 demands an explicit versioned layout); hiding it
   inside a KV store surrenders checksums, offsets, and the compression
   story.
4. **Memory-mapped I/O now** — deferred: readers load whole files while
   segments are small; access is already via offset slices with no
   mutation, so mmap drops in without API changes.

## Decision

- **Dictionary:** sorted `(field, term)` array with binary search (§16);
   prefix enumeration via lower bound for autocomplete later.
- **Publication:** build fully in memory → temp file → `fsync` → verify by
  reading back through the reader (counts + every posting list) → atomic
  rename → directory `fsync` → manifest append (itself temp + `fsync` +
  rename). A crash leaves only temp files.
- **Integrity:** header CRC, footer magic + file CRC, per-block CRCs,
  strict version/flags/dictionary-order checks. `open` fully verifies;
  anything malformed is `Error::Corrupt`, never a partial read.
- **Stored fields:** whole `Document` as JSON per doc (serde_json, already
  a dependency). A columnar stored-field encoding can replace it without
  touching postings when profiling justifies it.
- **Statistics:** doc count + per-field token totals per segment — the
  exact BM25 inputs (milestone 07). Global aggregation across segments is
  a sum, which keeps distributed IDF (milestone 13) honest.

## Tradeoffs

- Whole-file reads on open: fine for MB-scale segments, must become
  mmap/lazy-block reads before 100K-document corpora (§85).
- JSON stored docs inflate segments (see ADR 005: stored docs + dict are
  most of the 1.9 MB demo segment). Acceptable until snippets (milestone
  07) define the real stored-field workload.

## Consequences

- `IndexView` unifies `MemIndex` and `SegmentReader`; execution semantics
  are generation-independent and merging (§23, milestone 06) must preserve
  them (`merge == rebuild`, already tested at the mem level).
- Full byte layout documented in `docs/index-format.md`.
