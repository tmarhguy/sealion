# ADR 003 — In-memory inverted index and reference search engine

**Status:** accepted (milestone 03)
**Date:** 2026-10-01

## Context

SeaLion needs to search *now* (spec build order: in-memory index before
disk segments), plus the mandatory correctness oracle (§26) that every
later optimization is verified against.

## Alternatives considered

1. **Skip straight to persistent segments** — rejected: delays first search
   and leaves no clean seam between semantics (what matches) and storage
   (how postings persist). The in-memory index defines the semantics that
   segments must later preserve bit-for-bit (§96: `merge == rebuild`).
2. **`HashMap<Term, Vec>` posting store (chosen shape, BTreeMap keyed
   `(Field, term)`)** — per-field dictionaries keep `title:compiler`
   scoping exact and ranking weights independent (§15, §29). `BTreeMap`
   gives deterministic iteration for tests and dumps; a hash map would be
   faster but nondeterministic in diagnostics.
3. **Reference engine as a separate binary** — rejected: a library function
   over the same `Query` AST and `Document`s keeps the two engines honest
   by construction and runs inside the same test processes.

## Decision

- **`MemIndex`:** `(Field, term) → postings sorted by DocId`, each posting
  carrying sorted positions. Re-adding an ID replaces its postings;
  removal drops them. Field lengths (emitted token counts) stored now for
  BM25 later. Out-of-order inserts re-sort touched lists.
- **Boolean semantics:** `AND` = sorted intersection (smallest-first),
  `OR` = sorted union, `NOT` = complement over all docs. Unscoped terms
  match if *any* field contains the term; multi-token raw query text becomes
  a conjunction per field, disjoined across fields (the only correct
  reading under divergent per-field analysis).
- **`reference_search`:** re-analyzes stored documents and evaluates the
  query with zero index structures — `O(docs × query)` by design, never on
  a serving path.
- **Verification:** hand cases + deterministic LCG fuzz (200 random
  queries/corpora, no new deps) + `deleted docs never appear` +
  `incremental == rebuild`, in unit and integration tests.

## Tradeoffs

- Single-process, memory-bound, no persistence — all addressed by
  milestones 04–06 (segments, compression, merging). The API seam
  (`postings(field, term)`, sorted DocIds) is what segments must honor.
- No ranking yet: Boolean retrieval only. BM25 arrives in milestone 07 on
  top of these postings (frequencies and positions already stored).

## Consequences

- Optimized top-k (§37–40) must reproduce indexed Boolean candidate sets
  exactly; the reference engine is the arbiter (§41).
- Segment format work (milestone 04) inherits these semantics: fielded
  terms, gapped positions, replace-on-add.
