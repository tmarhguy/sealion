//! Link authority (spec §54, milestone 18): PageRank over the internal
//! outlink graph, normalized to [0, 1].
//!
//! Edges come from `metadata["outlinks"]` (newline-joined, written by the
//! crawler and HTML ingestion); only targets present in the corpus become
//! edges. Dangling nodes (no in-corpus outlinks) spread their rank evenly
//! (standard treatment). Scores normalize by the max so `authority_weight`
//! blends a bounded signal.

use std::collections::{BTreeMap, HashMap};

use sealion_core::document::{DocId, Document};

/// Build the internal link graph: DocId → outlinked DocIds in the corpus.
/// Duplicate targets collapse; self-links dropped.
pub fn build_graph(docs: &[Document]) -> BTreeMap<DocId, Vec<DocId>> {
    let by_url: HashMap<&str, DocId> = docs.iter().map(|d| (d.url.as_str(), d.id)).collect();
    let mut graph = BTreeMap::new();
    for d in docs {
        let mut targets = Vec::new();
        if let Some(raw) = d.metadata.get("outlinks") {
            for url in raw.split('\n').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(&id) = by_url.get(url) {
                    if id != d.id && !targets.contains(&id) {
                        targets.push(id);
                    }
                }
            }
        }
        targets.sort();
        graph.insert(d.id, targets);
    }
    graph
}

/// PageRank over `graph` (must include every node, possibly with empty
/// edge lists). Returns scores normalized by the max (best = 1.0).
/// Converges by fixed iteration count (50 default is plenty for test and
/// demo corpora; residual-based stopping is future work).
pub fn pagerank(
    graph: &BTreeMap<DocId, Vec<DocId>>,
    damping: f64,
    iterations: usize,
) -> HashMap<DocId, f64> {
    let n = graph.len();
    let mut rank: HashMap<DocId, f64> = HashMap::new();
    if n == 0 {
        return rank;
    }
    let ids: Vec<DocId> = graph.keys().copied().collect();
    for id in &ids {
        rank.insert(*id, 1.0 / n as f64);
    }
    let base = (1.0 - damping) / n as f64;
    for _ in 0..iterations.max(1) {
        let mut next: HashMap<DocId, f64> = HashMap::new();
        for id in &ids {
            next.insert(*id, base);
        }
        // Dangling mass spreads evenly.
        let mut dangling = 0.0;
        for id in &ids {
            if graph.get(id).map(Vec::is_empty).unwrap_or(true) {
                dangling += rank[id];
            }
        }
        let spread = damping * dangling / n as f64;
        for v in next.values_mut() {
            *v += spread;
        }
        for (src, targets) in graph {
            if targets.is_empty() {
                continue;
            }
            let share = damping * rank[src] / targets.len() as f64;
            for t in targets {
                *next.get_mut(t).expect("graph closed") += share;
            }
        }
        rank = next;
    }
    // Normalize by max → [0, 1].
    let max = rank.values().copied().fold(0.0f64, f64::max).max(1e-300);
    for v in rank.values_mut() {
        *v /= max;
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: u64, url: &str, outlinks: &[&str]) -> Document {
        let mut metadata = HashMap::new();
        if !outlinks.is_empty() {
            metadata.insert("outlinks".to_string(), outlinks.join("\n"));
        }
        Document {
            id: DocId(id),
            source: sealion_core::document::Source::Synthetic { name: "a".into() },
            url: url.into(),
            title: format!("t{id}"),
            headings: Vec::new(),
            body: "words here".into(),
            anchor_text: Vec::new(),
            metadata,
            timestamp: 0,
            language: "en".into(),
            content_hash: String::new(),
        }
    }

    #[test]
    fn hub_and_spokes_ranks_hub_highest() {
        // 0 links to 1,2,3; they link back to 0. 0 is the authority.
        let docs = vec![
            doc(0, "u://0", &["u://1", "u://2", "u://3"]),
            doc(1, "u://1", &["u://0"]),
            doc(2, "u://2", &["u://0"]),
            doc(3, "u://3", &["u://0"]),
        ];
        let g = build_graph(&docs);
        assert_eq!(g[&DocId(0)].len(), 3);
        let r = pagerank(&g, 0.85, 50);
        assert!((r[&DocId(0)] - 1.0).abs() < 1e-9, "{r:?}");
        assert!(r[&DocId(1)] < r[&DocId(0)]);
        // External targets and self-links are dropped.
        let docs = vec![doc(0, "u://0", &["u://0", "https://elsewhere.example/x"])];
        assert!(build_graph(&docs)[&DocId(0)].is_empty());
    }

    #[test]
    fn empty_graph_scores_nothing() {
        assert!(pagerank(&BTreeMap::new(), 0.85, 50).is_empty());
    }
}
