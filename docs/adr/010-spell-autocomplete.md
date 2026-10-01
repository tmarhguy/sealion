# ADR 010: Typo Correction and Autocomplete

Recorded after milestone 10 implementation. Applies to
`sealion-query::spell` and the `sealion complete` CLI surface.

## Context

Spec §43–47 requires correction over the vocabulary (no brute force),
candidate strategies (§44: BK-tree / SymSpell deletion dict / trie
traversal), explicit fuzziness (§45, already shipped in milestone 08),
and ranked prefix suggestions (§46–47) without user tracking.

## Decision

- **BK-tree** over the live per-field vocabulary (§44 pick): compact,
  dependency-free, exact within distance N. Chosen over the SymSpell
  deletion dictionary (faster queries but precomputes every deletion
  variant — memory-heavy per field) and trie traversal (needs a new
  persistent structure; the sorted-array dictionary stays). The tree is
  built per call from `IndexView::field_terms`; persisting it is
  milestone 16 work. Exactness vs the naive scan is fuzz-tested.
- **Exact Levenshtein** inside the tree: the capped distance used for
  early-exit elsewhere would violate the triangle inequality and break
  pruning, so the tree uses its own exact two-row implementation.
- **Ranking, no tracking**: corrections by (distance asc, df desc, term
  asc); completions by (df desc, term asc) over the sorted-dictionary
  scan. Deterministic, corpus-only signals.
- **Surfaces**: `sealion complete <prefix> [--field] [--limit]` (prefix
  normalized through the pipeline first); automatic `did you mean`
  on empty search results (best correction per query term, distance ≤ 2,
  each suggestion shown once). Displayed forms are normalized index
  terms, matching engine behavior everywhere else.
- **Fuzzy queries** keep the milestone 08 vocabulary-scan execution (now
  sharing correction semantics); BK-tree acceleration of that path is
  milestone 16 work.

## Consequences

- Typo UX works end-to-end (`distribted databse` → corrections) with no
  new dependencies and no format changes.
- Per-call tree builds are O(vocab) — fine now, profiled later.
