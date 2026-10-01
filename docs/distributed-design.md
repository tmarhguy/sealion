# Distributed Design

Milestones 13–15. Nodes are directories here
(`<data>/nodes/<node>/shard-<ii>/`); placement, file-copy, routing, and
failure mechanics are identical over a network — that transport boundary
is explicitly the next step, not this one. No consensus layer, per spec
§66: search is read-heavy, so replication copies immutable files.

## 1. Sharding (§60–61, milestone 13)

Document-based partitioning, `fnv1a64(doc_id) % shard_count` — stable,
content-independent, computable at index/delete/search time without
lookup. Shard count is fixed at `cluster init` (range assignment with
bounded movement is the documented alternative if counts must change).

Each shard copy keeps its own manifest (segments + tombstones), so
deletes route to the owning shard and merges stay shard-local.

## 2. Coordinator (§62–65, milestone 13)

Per query: open primaries → gather **global statistics** → parallel local
top-N under global stats (tokio `spawn_blocking`, readers are in-RAM) →
global merge (plain sort, scores comparable) → snippets.

Global stats (§63) are what make distribution exact rather than
approximate: N, per-field token totals, and per-term df summed across
live shards; the query expands once over the **union vocabulary** so
prefix/fuzzy resolve identically everywhere. Local top-N therefore equals
the global top-k restricted to that shard, and the merge equals a
single-index run — property-tested over Boolean shapes, phrases,
prefixes, and top-k values (`tests/cluster_equivalence.rs`).

Per-shard WAND is *not* wired into the coordinator yet: the local top-N
uses exhaustive scoring under global stats (correct; WAND-in-coordinator
is milestone 16 perf work).

## 3. Replicas and routing (§66–69, milestone 14)

`cluster init --replicas R` places copy j of shard i on node
`(i+j) % R`. Cluster indexing writes one segment per shard to its primary
(first healthy copy) and copies it verified to siblings. Routing
(`replica::Router`) is round-robin by default, least-outstanding on
request; observed latency is recorded for latency-aware routing later.

Health (§68): Healthy/Degraded/Offline per copy + generation +
last-heartbeat. Stale generations are never served. Copy open failure
fails over to the next healthy copy with a diagnostic; the query stays
complete (`partial: false`).

Partial semantics (§69): any unserved shard sets `partial: true` with a
per-shard diagnostic. Verified live: killing both copies of a shard
serves survivors with `partial: true`, never silent.

## 4. Rebalancing (§71, milestone 15)

`cluster move <shard> <dest-node> [--from src]`: copy segments →
verify every copy by opening it → register destination + drop source in
**one atomic topology store** → delete source files best-effort. Crash
before the store keeps old routing; crash after leaves a harmless
orphan. Queries serve throughout (old copy until the flip). Verified:
moved shards return identical hits.

## 5. CLI surface

`cluster init/status/move`, `shard list`, `node list`; `index`,
`search`, `index merge`, and `index delete` all route shard-aware
automatically when `cluster.json` exists (partitioned index, coordinator
search, per-shard merge, owning-shard delete).

## 6. Non-goals (yet)

Network transport, membership/consensus, generation-aware replica sync
beyond file copy, cross-shard transactions, per-shard WAND, cache layers
(milestone 16), failure-injection suite (milestone 17).
