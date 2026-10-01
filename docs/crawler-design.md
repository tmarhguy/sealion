# Crawler Design

Milestone 11. Pipeline: seeds → canonicalization → frontier → polite fetch
(robots, SSRF defense) → HTML extraction → dedup → `Document` → segment.

## 1. Frontier (`sealion-crawler/src/frontier.rs`)

Persistent priority scheduling. Each entry: URL, host, priority, depth,
retries, next-eligible time (unix ms), discovery source. Stored as JSON
beside the index (`crawl-state/frontier.json`), atomic temp-write + rename,
so `sealion crawl` resumes after Ctrl-C: re-running with the same seeds
re-queues only unseen URLs and continues where it stopped.

Fairness: `pop_eligible` prefers a different host than last served
(round-robin across same-priority entries), so one domain cannot consume
the crawler (§7). Higher priority first (seeds 100, depth-penalized links
50/40/…), eligible-time gating for backoff retries.

## 2. Canonicalization (`canonical.rs`, §9)

Conservative: lowercase scheme/host, strip default ports, drop fragments,
resolve dot segments, empty path → `/`. Query strings kept byte-identical
(tracking params *can* change content); true duplicates are caught by
content hashing instead. Non-http(s) and unparseable URLs rejected before
the frontier. `resolve()` filters `javascript:`/`mailto:`/`data:`/fragments
and absolutizes against the page URL.

## 3. Politeness (`fetch.rs`, §8)

- Per-host `next_eligible` = now + max(config `crawl_delay_ms`, robots
  `Crawl-delay`); `inflight` counter tracked (single-flight today, §5).
- `robots.txt` fetched once per host (miss/unreachable = allow all),
  parsed by our RFC 9309 subset parser (`robots.rs`): UA groups,
  Allow/Disallow longest-match, `*`/`$` patterns, Crawl-delay.
- HTTP 429 and 5xx → bounded retries with exponential backoff
  (`retry_backoff_ms << retries`, capped by `max_retries`); other 4xx fail
  fast with a warning.
- Client: reqwest with fetch timeout, redirect cap, gzip, configured
  User-Agent. Response cap via content-length pre-check + post-read check
  (`max_response_bytes`); true streaming truncation is future work — the
  timeout bounds pathological streams.

## 4. SSRF defense (§102)

Before fetching, literal IPs are checked directly and hostnames are
DNS-resolved with **every** address checked: loopback, private,
link-local, multicast, unspecified, broadcast, and documentation ranges
are refused. `allow_loopback` (tests only, never production) disables the
check for the local-server integration suite.

## 5. HTML extraction (`html.rs`, §11)

`scraper` supplies the DOM only. Ours: `<title>`, `h1–h6`, body text with
`script`/`style`/`nav`/`header`/`footer`/`noscript`/`svg` subtrees dropped,
`<a href>` + anchor text (resolved absolute), `<link rel=canonical>`,
meta/OG description.

**Images**: binaries are never fetched for indexing (non-HTML content
types return `SkippedContentType` before the body is read). Each `<img>`
contributes its `alt` text to the document body and its resolved `src` to
`metadata[image_refs]` (capped at 32, + `image_count`). Local corpus
`.png/.jpg/...` files stay in the skip counter. No OCR/EXIF — documented
non-goal for v1.

## 6. Dedup (`dedup.rs`, §10)

- Exact: FNV-1a64 over lowercased whitespace-collapsed content (hex).
- Near: 64-bit SimHash over word 5-shingles (SplitMix64-finalized FNV for
  uniform bit votes), Hamming ≤ 7 on texts ≥ 25 words. Operating point
  measured: 1–2 word edits in 60-word pages score 4–6, unrelated pages
  ~35. Short texts use exact hashing only (too few shingles to be stable).
- First sighting wins; counters (`exact_dups`, `near_dups`, `unique`)
  print per crawl run. Resume path seeds the exact set from indexed
  content hashes.

## 7. Documents and indexing

Stable IDs: `FNV(canonical URL)` — recrawls replace the same ID (update
semantics via milestone 06 generations). `Source::Web`, title falls back
to URL when absent, `metadata[crawl_source/description/image_*]`.
`sealion crawl URL... [--depth N] [--pages N]` writes one segment per run
and prints fetch/dedup counters; `restrict_to_seed_domains` (default true)
keeps crawls on-topic. Local `.html`/`.htm` files go through the same
extractor in `sealion index`.

## 8. Metrics (per run)

`pages, failed, retried, robots_denied, skipped_content, exact_dups,
near_dups`, printed by the CLI. Prometheus/OTel export is milestone 19+.

## 9. Known limitations

- Sequential fetching (per-host politeness dominates anyway); cross-host
  parallelism is milestone 13+ work.
- No JS rendering, no sitemap.xml, no Last-Modified/ETag revalidation,
  no per-host connection caps beyond the global semaphore budget.
- Language always `en` (detection is future work).
