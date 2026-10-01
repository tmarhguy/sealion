//! Sharding (spec §60–61): document-based partitioning with stable
//! deterministic assignment.
//!
//! Assignment is `fnv1a64(doc_id) % shard_count` over the raw DocId —
//! stable across runs, independent of content, and trivially computable at
//! index and delete time. Shard copies live at
//! `<data>/nodes/<node>/shard-<ii>/`, each with its own manifest (tombstones
//! stay shard-local, which is exactly where deletes route).
//!
//! Range/consistent-hashing assignment stays an option if shard-count
//! changes need bounded movement; today shard count is fixed at
//! `cluster init` time (documented in `docs/distributed-design.md`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sealion_core::document::DocId;
use serde::{Deserialize, Serialize};

/// Deterministic shard assignment (§61).
pub fn shard_of(id: DocId, shard_count: usize) -> usize {
    debug_assert!(shard_count >= 1);
    // FNV-1a over the raw id (stable, content-independent).
    let mut h: u64 = 0xcbf29ce484222325;
    for b in id.0.to_le_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    (h % shard_count as u64) as usize
}

/// Directory holding one shard copy's segments.
pub fn shard_copy_dir(data_dir: &Path, node: &str, shard: usize) -> PathBuf {
    data_dir
        .join("nodes")
        .join(node)
        .join(format!("shard-{shard:02}"))
}

/// Copy health (§68).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyHealth {
    Healthy,
    Degraded,
    Offline,
}

/// One hosted shard copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardCopy {
    pub shard: usize,
    pub node: String,
    /// Manifest generation last observed (stale replicas are skipped, §68).
    pub generation: u64,
    pub health: CopyHealth,
    pub last_heartbeat_ms: u64,
}

/// Cluster topology: the routing table (§62–63, §68).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    pub shard_count: usize,
    pub nodes: Vec<String>,
    pub copies: Vec<ShardCopy>,
}

impl Topology {
    pub const FILE: &'static str = "cluster.json";

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(Self::FILE)
    }

    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(Self::path(data_dir)).map_err(|e| format!("cluster.json: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("cluster.json corrupt: {e}"))
    }

    pub fn store(&self, data_dir: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let tmp = data_dir.join("cluster.json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, Self::path(data_dir)).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Healthy copies for a shard, primaries first (insertion order).
    pub fn healthy_copies(&self, shard: usize) -> Vec<&ShardCopy> {
        self.copies
            .iter()
            .filter(|c| c.shard == shard && c.health == CopyHealth::Healthy)
            .collect()
    }

    /// Shards with no healthy copy (→ `partial = true`, §69).
    pub fn unavailable_shards(&self) -> Vec<usize> {
        (0..self.shard_count)
            .filter(|s| self.healthy_copies(*s).is_empty())
            .collect()
    }
}

/// Global collection statistics for comparable cross-shard BM25 (§63).
#[derive(Debug, Clone, Default)]
pub struct GlobalStats {
    pub n: usize,
    pub field_totals: [u64; 4],
    pub df: BTreeMap<(sealion_core::field::Field, String), usize>,
    pub max_timestamp: u64,
}

impl GlobalStats {
    /// Merge one shard's contribution.
    pub fn add_shard(
        &mut self,
        n: usize,
        totals: [u64; 4],
        df: BTreeMap<(sealion_core::field::Field, String), usize>,
        max_timestamp: u64,
    ) {
        self.n += n;
        for (i, t) in totals.iter().enumerate() {
            self.field_totals[i] += t;
        }
        for (k, v) in df {
            *self.df.entry(k).or_insert(0) += v;
        }
        self.max_timestamp = self.max_timestamp.max(max_timestamp);
    }

    /// As query-engine `CollectionStats` (avgdl from global totals).
    pub fn as_collection_stats(
        &self,
        scoring_terms: &[(sealion_core::field::Field, String)],
    ) -> sealion_query::rank::CollectionStats {
        use sealion_core::field::Field;
        let n = self.n.max(1);
        let mut avg = [0f64; 4];
        for f in Field::ALL {
            avg[f.index()] = self.field_totals[f.index()] as f64 / n as f64;
        }
        let mut df = std::collections::BTreeMap::new();
        for (f, t) in scoring_terms {
            df.insert(
                (*f, t.clone()),
                self.df.get(&(*f, t.clone())).copied().unwrap_or(0),
            );
        }
        sealion_query::rank::CollectionStats {
            n,
            df,
            avg,
            max_timestamp: self.max_timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_is_stable_and_covering() {
        for id in [0u64, 1, 7, 999, u64::MAX - 1] {
            let a = shard_of(DocId(id), 4);
            assert_eq!(a, shard_of(DocId(id), 4));
            assert!(a < 4);
        }
        // All shards get traffic over a range.
        let mut seen = [false; 4];
        for id in 0..100u64 {
            seen[shard_of(DocId(id), 4)] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn unavailable_shards_detected() {
        let t = Topology {
            shard_count: 2,
            nodes: vec!["n0".into()],
            copies: vec![ShardCopy {
                shard: 0,
                node: "n0".into(),
                generation: 1,
                health: CopyHealth::Offline,
                last_heartbeat_ms: 0,
            }],
        };
        assert_eq!(t.unavailable_shards(), vec![0, 1]);
    }
}
