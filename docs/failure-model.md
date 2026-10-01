# Failure Model

Milestone 17. What can break, what SeaLion does, and which test proves it
(§99–100, §92).

## 1. Storage and crash safety

| Injection | Expected behavior | Test |
|---|---|---|
| Crash mid-segment-write | only temp files; manifest never names them; old generation serves | `failed_publication_leaves_no_visible_segment` + `tests/failure.rs::unusable_data_dir_fails_cleanly` |
| Crash mid-merge | temp segment orphaned; manifest unadvanced; old segments serve | `tests/failure.rs::failed_merge_leaves_old_generation_intact` |
| Crash between delete and merge | tombstone in manifest keeps the doc hidden across restarts | `tests/failure.rs::tombstone_survives_without_merge` |
| Any single-byte segment mutation (any region) | open fails (header/file/block CRC or structural check) | `tests/adversarial.rs::corruption_detected_in_every_region` |
| Garbage/truncated segment, garbage manifest | `Err`, never panic, never silent | `tests/adversarial.rs` |
| Disk full / unwritable dir | clean `Err`, no partial manifest | `unusable_data_dir_fails_cleanly` (file-as-dir; root-proof) |
| Shard move crash (before store) | old routing live | by construction (store is the flip); `moved_shard_preserves_results` covers the happy path |

## 2. Distributed failures (§92, §69)

| Injection | Expected behavior | Proof |
|---|---|---|
| One replica copy lost | transparent failover, `partial: false`, full results + diagnostic | `single_copy_loss_fails_over_transparently` + CLI demo |
| All copies of a shard lost | survivors served, `partial: true`, per-shard diagnostics | `dead_shard_is_partial_never_silent` + CLI demo |
| Query-node task failure | shard dropped from merge, `partial: true` | coordinator `per_shard_ms` accounting |
| Slow peer | per-shard latency recorded; least-outstanding routing exists; latency-aware policy is future work | `per_shard_ms` in every cluster result |

## 3. Input and crawl failures

| Injection | Expected behavior | Proof |
|---|---|---|
| Malformed query text | `InvalidQuery` or `MatchNothing`, never panic | `tests/adversarial.rs` (2000 garbage inputs) |
| Malformed HTML / binary mislabeled | best-effort extraction, never panic | `html::garbage_html_never_panics`, local-server binary skip |
| HTTP 429 / 5xx | bounded retries with backoff, then drop + count | `CrawlStats::{retried, failed}` |
| robots deny | skip + count | local-server `/blocked` case |
| Loopback/private fetch | refused unless `allow_loopback` (tests only) | `ssrf_check` (unit-covered via integration config) |

## 4. Soak (§101)

`tests/failure.rs::soak_mixed_workload` (`#[ignore]`, `SEALION_SOAK_ITERS`
configurable): sustained index/search/merge cycles asserting doc-count
consistency and latency non-drift. CI runs the fast suites; soak + future
`cargo fuzz` targets are nightly work (see `docs/correctness.md` §2).
