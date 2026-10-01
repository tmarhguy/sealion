//! Replicas, routing, and health (spec §66–69).
//!
//! Each shard has 1+ copies on different nodes (directories here; the
//! placement and file-copy mechanics are identical over a network).
//! Search is read-heavy, so replication copies immutable segment files —
//! no consensus layer (§66). Routing picks a healthy copy per shard:
//! round-robin by default, least-outstanding when loads skew. Copies with
//! stale generations or errors are skipped and reported; a query that
//! misses any shard is `partial = true`, never silently complete (§69).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::shard::{CopyHealth, ShardCopy, Topology};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Routing strategies (§67).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteStrategy {
    #[default]
    RoundRobin,
    LeastOutstanding,
}

/// In-memory router: per-shard round-robin cursors + inflight counters +
/// observed latency (latency-aware routing is a documented next step that
/// consumes `latency_ms`).
#[derive(Debug, Default)]
pub struct Router {
    strategy: RouteStrategy,
    cursors: HashMap<usize, usize>,
    inflight: HashMap<(usize, String), usize>,
    latency_ms: HashMap<(usize, String), f64>,
}

impl Router {
    pub fn new(strategy: RouteStrategy) -> Self {
        Self {
            strategy,
            ..Self::default()
        }
    }

    /// Pick a healthy copy for `shard`. Returns the selection (caller opens
    /// it; success/failure is reported back via note_*).
    pub fn pick(&mut self, topo: &Topology, shard: usize) -> Option<ShardCopy> {
        let healthy = topo.healthy_copies(shard);
        if healthy.is_empty() {
            return None;
        }
        let idx = match self.strategy {
            RouteStrategy::RoundRobin => {
                let c = self.cursors.entry(shard).or_insert(0);
                let i = *c % healthy.len();
                *c = c.wrapping_add(1);
                i
            }
            RouteStrategy::LeastOutstanding => {
                let mut best = 0;
                let mut best_load = usize::MAX;
                for (i, cp) in healthy.iter().enumerate() {
                    let load = self
                        .inflight
                        .get(&(shard, cp.node.clone()))
                        .copied()
                        .unwrap_or(0);
                    if load < best_load {
                        best_load = load;
                        best = i;
                    }
                }
                best
            }
        };
        let chosen = healthy[idx].clone();
        *self
            .inflight
            .entry((shard, chosen.node.clone()))
            .or_insert(0) += 1;
        Some(chosen)
    }

    pub fn note_success(&mut self, shard: usize, node: &str) {
        if let Some(n) = self.inflight.get_mut(&(shard, node.to_string())) {
            *n = n.saturating_sub(1);
        }
    }

    pub fn note_failure(&mut self, shard: usize, node: &str) {
        self.note_success(shard, node);
    }

    pub fn note_latency(&mut self, shard: usize, node: &str, ms: f64) {
        self.latency_ms.insert((shard, node.to_string()), ms);
    }
}

/// Mark a copy's health (heartbeat path, §68). Heartbeats also refresh
/// `last_heartbeat_ms`; copies silent past `stale_after_ms` report
/// Degraded (callers decide promotion/demotion policy).
pub fn heartbeat(topo: &mut Topology, shard: usize, node: &str, generation: u64, healthy: bool) {
    if let Some(cp) = topo
        .copies
        .iter_mut()
        .find(|c| c.shard == shard && c.node == node)
    {
        cp.generation = generation;
        cp.last_heartbeat_ms = now_ms();
        cp.health = if healthy {
            CopyHealth::Healthy
        } else {
            CopyHealth::Offline
        };
    }
}

/// Copy segment files from `src` to `dst` and verify each by opening it
/// (checksum-verified reader). Used by cluster indexing (replication) and
/// rebalancing (migration). Returns files copied.
pub fn copy_segments_verified(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<Vec<String>, String> {
    use sealion_index::segment::manifest::Manifest;
    use sealion_index::segment::reader::SegmentReader;
    let manifest = Manifest::load_or_new(src).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut copied = Vec::new();
    for name in &manifest.segments {
        let bytes = std::fs::read(src.join(name)).map_err(|e| format!("{name}: {e}"))?;
        std::fs::write(dst.join(name), &bytes).map_err(|e| format!("{name}: {e}"))?;
        // Verify the copy before trusting it.
        SegmentReader::open(&dst.join(name)).map_err(|e| format!("{name} copy corrupt: {e}"))?;
        copied.push(name.clone());
    }
    // Replicate the manifest (tombstones included) after segments verify.
    let mut dst_manifest = Manifest::load_or_new(dst).map_err(|e| e.to_string())?;
    for name in &manifest.segments {
        if !dst_manifest.segments.contains(name) {
            dst_manifest.segments.push(name.clone());
        }
    }
    for t in &manifest.deleted {
        if !dst_manifest.deleted.contains(t) {
            dst_manifest.deleted.push(*t);
        }
    }
    dst_manifest.generation = dst_manifest.generation.max(manifest.generation);
    dst_manifest.store(dst).map_err(|e| e.to_string())?;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shard::Topology;

    fn topo() -> Topology {
        Topology {
            shard_count: 1,
            nodes: vec!["a".into(), "b".into()],
            copies: vec![
                ShardCopy {
                    shard: 0,
                    node: "a".into(),
                    generation: 1,
                    health: CopyHealth::Healthy,
                    last_heartbeat_ms: 0,
                },
                ShardCopy {
                    shard: 0,
                    node: "b".into(),
                    generation: 1,
                    health: CopyHealth::Healthy,
                    last_heartbeat_ms: 0,
                },
            ],
        }
    }

    #[test]
    fn round_robin_alternates_and_skips_offline() {
        let mut t = topo();
        let mut r = Router::new(RouteStrategy::RoundRobin);
        assert_eq!(r.pick(&t, 0).unwrap().node, "a");
        assert_eq!(r.pick(&t, 0).unwrap().node, "b");
        heartbeat(&mut t, 0, "a", 1, false);
        assert_eq!(r.pick(&t, 0).unwrap().node, "b");
        assert_eq!(r.pick(&t, 0).unwrap().node, "b");
    }

    #[test]
    fn least_outstanding_prefers_idle() {
        let t = topo();
        let mut r = Router::new(RouteStrategy::LeastOutstanding);
        let first = r.pick(&t, 0).unwrap().node;
        // First pick holds a lease (no note_success), so second differs.
        let second = r.pick(&t, 0).unwrap().node;
        assert_ne!(first, second);
    }
}
