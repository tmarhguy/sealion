# ADR 006: Generations, Tombstones, and Segment Merging

Recorded after milestone 06 implementation. Applies to `sealion-index`.

## Context

Segments are immutable (§17). Re-indexing a corpus appends a new segment;
without generation semantics, old versions resurrect and deletes are
impossible without a full rebuild (§24). Merging must preserve search
results (§23) while queries keep running.

## Decision

- **Manifest order is generation order** (oldest → newest). `Manifest.deleted: Vec<u64>` holds tombstones (serde-defaulted, old manifests load).
- **`MultiSegmentView`** (in `view.rs`): newest segment owns each DocId; postings shadow per-doc (stale terms never resurrect); tombstones invisible. `field_token_total` sums stored stats minus stale/tombstoned versions (re-analyzed, usually few).
- **Merge** (`merge.rs::merge_all_segments`): live set (newest wins, tombstones out) → `MemIndex` → crash-safe `write_segment` → single atomic `manifest.store` swapping N segments for 1 → best-effort old-file deletion. Crash before store keeps old generation; crash after store leaves harmless orphans.
- **CLI**: `sealion index merge`, `sealion index delete <id>`; search/stats tombstone-aware.

## Alternatives

- Per-segment deletion bitmaps in the file format: rejected — format churn for v1; manifest tombstones suffice until scale demands it.
- Eager physical deletion on `delete`: rejected — violates immutability; tombstone + merge is the standard LSM approach.

## Consequences

- ADD/UPDATE/DELETE work without rebuild; `merge == clean rebuild` verified by `tests/generations.rs`.
- `print_stats` shows `tombstones:`; multi-segment totals are raw sums (live totals available via view).
