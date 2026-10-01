//! Query-result cache (spec §72–73, milestone 16).
//!
//! Key = (raw query string, top_k, ranking knobs, index generation). A new
//! incompatible generation invalidates stale results (§73) — the caller
//! passes the manifest generation, and any mismatch simply misses.
//!
//! In-memory LRU for long-lived processes (benchmark harness, API server).
//! One-shot CLI searches don't use it (nothing to reuse across execs);
//! the coordinator likewise stays uncached until the server lands.

use std::collections::{HashMap, VecDeque};

use crate::rank::RankedHit;

/// Cache key: everything that can change a ranking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    query: String,
    top_k: usize,
    ranking: (u64, u64, u64, u64, u64, u64),
    generation: u64,
}

fn bits(f: f64) -> u64 {
    f.to_bits()
}

/// LRU query-result cache with hit/miss/eviction counters (§83).
#[derive(Debug)]
pub struct QueryCache {
    cap: usize,
    map: HashMap<Key, Vec<RankedHit>>,
    order: VecDeque<Key>,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

impl QueryCache {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    fn key(
        query: &str,
        top_k: usize,
        ranking: &sealion_core::config::RankingConfig,
        generation: u64,
    ) -> Key {
        Key {
            query: query.to_string(),
            top_k,
            ranking: (
                bits(ranking.bm25_k1),
                bits(ranking.bm25_b),
                bits(ranking.title_weight),
                bits(ranking.heading_weight),
                bits(ranking.body_weight),
                bits(ranking.anchor_weight),
            ),
            generation,
        }
    }

    pub fn get(
        &mut self,
        query: &str,
        top_k: usize,
        ranking: &sealion_core::config::RankingConfig,
        generation: u64,
    ) -> Option<Vec<RankedHit>> {
        let key = Self::key(query, top_k, ranking, generation);
        match self.map.get(&key) {
            Some(hits) => {
                self.hits += 1;
                // Refresh recency.
                if let Some(pos) = self.order.iter().position(|k| k == &key) {
                    self.order.remove(pos);
                    self.order.push_back(key);
                }
                Some(hits.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    pub fn put(
        &mut self,
        query: &str,
        top_k: usize,
        ranking: &sealion_core::config::RankingConfig,
        generation: u64,
        hits: Vec<RankedHit>,
    ) {
        let key = Self::key(query, top_k, ranking, generation);
        if self.map.contains_key(&key) {
            return;
        }
        while self.map.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
                self.evictions += 1;
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, hits);
    }

    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranking() -> sealion_core::config::RankingConfig {
        sealion_core::config::RankingConfig::default()
    }

    #[test]
    fn hits_misses_and_generation_gating() {
        let mut c = QueryCache::new(4);
        assert!(c.get("compiler", 10, &ranking(), 1).is_none());
        c.put("compiler", 10, &ranking(), 1, Vec::new());
        assert!(c.get("compiler", 10, &ranking(), 1).is_some());
        // New generation (reindex/merge) misses: stale results never served.
        assert!(c.get("compiler", 10, &ranking(), 2).is_none());
        // Different top_k is a different key.
        assert!(c.get("compiler", 5, &ranking(), 1).is_none());
        assert!((c.hit_rate() - 1.0 / 4.0).abs() < 1e-12);
    }

    #[test]
    fn lru_evicts_oldest() {
        let mut c = QueryCache::new(2);
        c.put("a", 10, &ranking(), 1, Vec::new());
        c.put("b", 10, &ranking(), 1, Vec::new());
        c.put("c", 10, &ranking(), 1, Vec::new());
        assert_eq!(c.len(), 2);
        assert_eq!(c.evictions, 1);
        assert!(c.get("a", 10, &ranking(), 1).is_none());
        assert!(c.get("b", 10, &ranking(), 1).is_some());
    }
}
