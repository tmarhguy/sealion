# SeaLion Segment Format v1 (`*.seal`)

Versioned persistent layout for immutable index segments (spec §18).
All integers are little-endian. Offsets are absolute file positions.
See `docs/adr/004-segments.md` for rationale and
`docs/adr/005-postings-codec.md` for compression numbers.

```text
+-------------------------------+
| header                  60 B  |
+-------------------------------+
| postings + positions blocks   |  <-- dictionary entries point here
+-------------------------------+
| dictionary  (count + entries) |
+-------------------------------+
| document metadata (stored)    |
+-------------------------------+
| statistics                    |
+-------------------------------+
| footer                  16 B  |
+-------------------------------+
```

## Header (60 bytes)

| offset | size | field | value |
|---|---|---|---|
| 0 | 8 | magic | `SEALION1` |
| 8 | 4 | version | `1` (readers reject anything else) |
| 12 | 4 | flags | bit 0: compressed postings; bit 1: compressed positions (unknown bits rejected) |
| 16 | 8 | doc_count | documents in this segment |
| 24 | 8 | dict_count | fielded terms |
| 32 | 8 | dict_offset | dictionary region start |
| 40 | 8 | meta_offset | metadata region start |
| 48 | 8 | stats_offset | statistics region start |
| 56 | 4 | header_crc | CRC32 of bytes 0..56 |

## Postings block (per term)

Compressed (`flags & 1`): `encode_deltas(doc_ids)` — varint count,
first DocID raw, then positive delta varints. Strictly increasing is
enforced on decode; violations are corruption.

Raw (benchmark only): u64 count + that many u64 DocIDs.

## Positions block (per term)

One position list per posting, in posting (DocID) order.
Compressed: concatenated `encode_deltas(positions)` lists.
Raw: per posting, u64 length + u64 positions.

## Dictionary

u64 entry count, then entries sorted by `(field, term)`:

| field | size |
|---|---|
| field id | 1 (`0`=title `1`=heading `2`=body `3`=anchor) |
| term_len + term bytes | 4 + N (UTF-8) |
| doc_freq | 8 |
| postings_offset / postings_len | 8 + 8 |
| positions_offset / positions_len | 8 + 8 |
| first_doc / last_doc | 8 + 8 (Block-Max WAND inputs, §22) |
| postings_crc / positions_crc | 4 + 4 |

## Document metadata

u64 doc count, then per document: u64 DocID + u32 JSON length + JSON
bytes (full `Document`). IDs must match the stored document's own ID.

## Statistics

u64 doc count, then 4×u64 emitted-token totals in canonical field order
(title, heading, body, anchor). Average field length = total / doc_count.

## Footer (16 bytes)

| offset | size | field |
|---|---|---|
| 0 | 8 | magic `SEALION1` |
| 8 | 4 | file_crc: CRC32 of all preceding bytes |
| 12 | 4 | reserved (0) |

## Manifest (`manifest.json`)

Sibling file in the data directory: `{ "generation": N, "segments":
[...] }`. Sole mutable file; published via write-temp + fsync + atomic
rename. Readers list segments in publication order.

## Versioning policy

Bump `version` for any layout change; readers reject unknown versions
loudly. New compression kinds use new `flags` bits (old readers reject
unknown bits rather than misreading them).
