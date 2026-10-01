# ADR 008: Query Parser, Planner, Filters, Intersections

Recorded after milestone 08 implementation. Applies to
`sealion-query::{parser,execute,query,rank}`.

## Context

Queries were built via `term_raw` only (AND of tokens / OR over fields).
Spec §30 requires phrases, AND/OR/NOT, `field:`, `site:`, `prefix*`;
§32 requires cost-based planning; §33 efficient intersections; §45 fuzzy.

## Decision

- **Grammar** (`parser.rs`): `NOT` > implicit/explicit `AND` > `OR`,
  parens, double-quoted phrases, `field:term` (title/heading/body/anchor),
  `site:host` (URL substring filter on stored docs), `prefix*`
  (normalized prefix), `term~N` (N=1–2, default 1). Unknown `field:` falls
  back to whole-token term. Empty → `MatchNothing`. Leaves capped by
  `max_query_terms` (§102).
- **AST**: new `Prefix{field,prefix}`, `Fuzzy{field,term,distance}`,
  `Site(host)` variants. `terms()` includes prefix/fuzzy literals;
  `leaf_count()` enforces limits.
- **Execution**: prefix/fuzzy expand against live vocabulary
  (`IndexView::field_terms`, added for all three views); site filters stored
  URLs. Reference oracle extended identically — agreement tests cover all
  new variants.
- **Ranking**: `expand_terms()` resolves prefix/fuzzy to concrete scoped
  terms so `comp*` scores/highlights its expansions (previously 0.0).
- **Planner**: `And` arms ordered by df-based `estimate_len`, cheapest
  first with early exit. Intersections auto-select linear vs galloping
  (≥8× skew threshold); both exact, equivalence-tested.

## Alternatives

- Stemming-aware prefix via trie/FST: deferred to milestone 10
  (autocomplete structures); sorted-array scan is exact and fast enough now.
- BK-tree fuzzy: deferred to milestone 10; capped-Levenshtein vocabulary
  scan is the correct baseline.

## Consequences

- CLI parses the full language; `--explain` shows expanded term scores.
- `field_terms` is a new required `IndexView` method (all impls updated).
- Galloping threshold (8×) is a documented heuristic, not yet profiled —
  profiling in milestone 16 may retune it.
