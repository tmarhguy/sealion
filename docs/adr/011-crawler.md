# ADR 011: Polite Crawler With Own Semantics on Borrowed Transport

Recorded after milestone 11 implementation. Applies to `sealion-crawler`.

## Context

Spec §2 forbids search-engine frameworks but allows HTTP/HTML libraries;
§6–11 require frontier, politeness, canonicalization, dedup, and field
extraction to be ours.

## Decision

- **Borrow**: `reqwest` (transport: timeouts, redirects, gzip),
  `scraper` (DOM parsing), `url` 2.x (parsing/joining only — upgraded
  from the stale 0.5 API already in the tree).
- **Own**: frontier scheduling + persistence, RFC 9309-subset robots
  parser, conservative canonicalizer, exact + SimHash dedup, boilerplate
  suppression, SSRF DNS-range defense, retry/backoff, domain policy.
- **Images**: alt-into-body + srcs-in-metadata; binaries never fetched.
  Local image files remain skip-counted.
- Sequential fetch loop; parallelism deferred to distribution work.

## Alternatives

- `robotparser` crate: rejected — parsing is the spec'd work, and ours
  is 150 lines with edge-case tests.
- Headless rendering (JS): rejected for v1 — cost and complexity with
  no relevance case yet.

## Consequences

- `web → crawler → index → search` works end-to-end (verified against a
  local server: robots deny, redirect, depth cap, binary skip, alt text).
- New surface: `sealion crawl`, `.html` corpus ingestion, `crawl-state/`.
