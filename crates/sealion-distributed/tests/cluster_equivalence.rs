//! Cluster ≡ single-index equivalence (§63–65) + failover + rebalance.
//!
//! The flagship distributed property: sharded search under global stats
//! returns exactly what one unsharded index returns — same docs, scores,
//! order — across Boolean shapes, phrases, prefixes, and top-k values.
//! Plus: dead shards yield `partial = true` (never silent), and shard
//! moves preserve results.

use std::collections::HashMap;

use sealion_core::config::{AnalysisConfig, RankingConfig, StemmerKind};
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_distributed::replica::Router;
use sealion_distributed::shard::{shard_copy_dir, shard_of, CopyHealth, ShardCopy, Topology};
use sealion_distributed::{distributed_search, CoordinatorResult};
use sealion_index::mem_index::MemIndex;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_query::query::Query;
use sealion_query::rank::ranked_search;

fn cfgs() -> (AnalysisConfig, RankingConfig) {
    (
        AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        },
        RankingConfig::default(),
    )
}

fn doc(id: u64, title: &str, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic { name: "cl".into() },
        url: format!("test://{id}"),
        title: title.into(),
        headings: Vec::new(),
        body: body.into(),
        anchor_text: Vec::new(),
        metadata: Default::default(),
        timestamp: 0,
        language: "en".into(),
        content_hash: String::new(),
    }
}

fn corpus() -> Vec<Document> {
    let bodies = [
        "compiler optimization passes and register allocation",
        "bytecode interpreter loops with dispatch tables",
        "distributed database systems replicate shards",
        "raft consensus elects leaders for log replication",
        "compiler backend generates machine code",
        "javascript event loop and async runtimes",
        "storage engines compress postings blocks",
        "database query planning and cost estimates",
        "network partitions and partial failure",
        "autocomplete tries rank prefix suggestions",
        "typo correction with edit distance candidates",
        "shard rebalancing moves immutable segments",
    ];
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, b)| doc(i as u64, &format!("doc {i}"), b))
        .collect()
}

fn queries() -> Vec<Query> {
    vec![
        Query::term(None, "compiler"),
        Query::term(Some(Field::Body), "compiler"),
        Query::and(vec![
            Query::term(None, "compiler"),
            Query::term(None, "optimization"),
        ]),
        Query::or(vec![
            Query::term(None, "compiler"),
            Query::term(None, "database"),
        ]),
        Query::and(vec![
            Query::term(None, "systems"),
            Query::negate(Query::term(None, "javascript")),
        ]),
        Query::Phrase {
            field: Field::Body,
            terms: vec!["database".into(), "systems".into()],
        },
        Query::Prefix {
            field: None,
            prefix: "compil".into(),
        },
        Query::term(None, "the"),
    ]
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sealion-cluster-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Single segment with all docs (reference side).
fn write_single(dir: &std::path::Path, docs: &[Document], a: &AnalysisConfig) -> SegmentReader {
    let mut idx = MemIndex::new(a.clone());
    for d in docs {
        idx.add_document(d.clone());
    }
    let meta = write_segment(dir, &idx, true).unwrap();
    SegmentReader::open(&dir.join(&meta.file_name)).unwrap()
}

/// Sharded layout + topology with one copy per shard on node-0.
fn write_cluster(
    data_dir: &std::path::Path,
    docs: &[Document],
    a: &AnalysisConfig,
    shards: usize,
) -> Topology {
    let mut per_shard: HashMap<usize, MemIndex> = HashMap::new();
    for d in docs {
        let s = shard_of(d.id, shards);
        per_shard
            .entry(s)
            .or_insert_with(|| MemIndex::new(a.clone()))
            .add_document(d.clone());
    }
    // Write even empty shards (uniform topology, zero-doc segments).
    let mut copies = Vec::new();
    for s in 0..shards {
        let dir = shard_copy_dir(data_dir, "node-0", s);
        std::fs::create_dir_all(&dir).unwrap();
        let idx = per_shard
            .remove(&s)
            .unwrap_or_else(|| MemIndex::new(a.clone()));
        // Empty MemIndex still writes a valid (empty) segment.
        let _ = write_segment(&dir, &idx, true).unwrap();
        copies.push(ShardCopy {
            shard: s,
            node: "node-0".into(),
            generation: 1,
            health: CopyHealth::Healthy,
            last_heartbeat_ms: 0,
        });
    }
    let topo = Topology {
        shard_count: shards,
        nodes: vec!["node-0".into()],
        copies,
    };
    topo.store(data_dir).unwrap();
    topo
}

fn analysis_cfg() -> AnalysisConfig {
    cfgs().0
}

#[tokio::test]
async fn cluster_matches_single_index() {
    let (a, r) = cfgs();
    let docs = corpus();
    let single_dir = tmpdir("single");
    let seg = write_single(&single_dir, &docs, &a);
    let data_dir = tmpdir("cluster");
    let topo = write_cluster(&data_dir, &docs, &a, 4);
    assert_eq!(topo.shard_count, 4);

    for q in queries() {
        for top_k in [1, 3, 10] {
            let want = ranked_search(&seg, &a, &r, &q, top_k).unwrap();
            let mut router = Router::default();
            let got: CoordinatorResult =
                distributed_search(&data_dir, &topo, &mut router, &a, &r, &q, top_k).await;
            assert!(!got.partial, "unexpected partial on {q:?}");
            assert!(got.diagnostics.is_empty(), "{:?}", got.diagnostics);
            assert_eq!(got.hits.len(), want.len(), "len on {q:?} k={top_k}");
            for (x, y) in got.hits.iter().zip(want.iter()) {
                assert_eq!(x.doc, y.doc, "order on {q:?} k={top_k}");
                assert!(
                    (x.score - y.score).abs() < 1e-5,
                    "score {} on {q:?}",
                    x.doc.0
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&single_dir);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn single_copy_loss_fails_over_transparently() {
    let (a, r) = cfgs();
    let docs = corpus();
    let data_dir = tmpdir("failover-one");
    // Two copies per shard, then destroy one copy of shard 0.
    let mut topo = write_cluster(&data_dir, &docs, &a, 2);
    topo.nodes.push("node-1".into());
    for s in 0..2 {
        let src = shard_copy_dir(&data_dir, "node-0", s);
        let dst = shard_copy_dir(&data_dir, "node-1", s);
        std::fs::create_dir_all(&dst).unwrap();
        sealion_distributed::replica::copy_segments_verified(&src, &dst).unwrap();
        topo.copies.push(ShardCopy {
            shard: s,
            node: "node-1".into(),
            generation: 1,
            health: CopyHealth::Healthy,
            last_heartbeat_ms: 0,
        });
    }
    topo.store(&data_dir).unwrap();
    std::fs::remove_dir_all(shard_copy_dir(&data_dir, "node-0", 0)).unwrap();

    let q = Query::term(None, "compiler");
    let mut router = Router::default();
    let got = distributed_search(&data_dir, &topo, &mut router, &a, &r, &q, 10).await;
    assert!(!got.partial, "{:?}", got.diagnostics);
    assert!(
        got.diagnostics.iter().any(|d| d.contains("failing over")),
        "{:?}",
        got.diagnostics
    );
    // Same docs as the intact cluster.
    let single_dir = tmpdir("failover-single");
    let seg = write_single(&single_dir, &docs, &a);
    let want = ranked_search(&seg, &a, &r, &q, 10).unwrap();
    assert_eq!(got.hits.len(), want.len());
    for (x, y) in got.hits.iter().zip(want.iter()) {
        assert_eq!(x.doc, y.doc);
        assert!((x.score - y.score).abs() < 1e-5);
    }
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&single_dir);
}

#[tokio::test]
async fn dead_shard_is_partial_never_silent() {
    let (a, r) = cfgs();
    let docs = corpus();
    let data_dir = tmpdir("failover");
    let mut topo = write_cluster(&data_dir, &docs, &a, 4);
    // Kill every copy of shard 0.
    for c in topo.copies.iter_mut().filter(|c| c.shard == 0) {
        c.health = CopyHealth::Offline;
    }
    let q = Query::term(None, "compiler");
    let mut router = Router::default();
    let got = distributed_search(&data_dir, &topo, &mut router, &a, &r, &q, 10).await;
    assert!(got.partial, "dead shard must set partial");
    assert!(
        got.diagnostics.iter().any(|d| d.contains("shard 00")),
        "{:?}",
        got.diagnostics
    );
    // Survivors still served.
    assert!(!got.hits.is_empty());
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn moved_shard_preserves_results() {
    let (a, r) = cfgs();
    let docs = corpus();
    let data_dir = tmpdir("move");
    let mut topo = write_cluster(&data_dir, &docs, &a, 2);
    let q = Query::term(None, "compiler");
    let mut router = Router::default();
    let before = distributed_search(&data_dir, &topo, &mut router, &a, &r, &q, 10).await;
    assert!(!before.partial);

    let moved =
        sealion_distributed::rebalance::move_shard(&data_dir, &mut topo, 0, "node-0", "node-1")
            .unwrap();
    assert!(!moved.is_empty());
    assert!(!shard_copy_dir(&data_dir, "node-0", 0).exists());
    assert!(topo.nodes.contains(&"node-1".to_string()));

    let mut router = Router::default();
    let after = distributed_search(&data_dir, &topo, &mut router, &a, &r, &q, 10).await;
    assert!(!after.partial, "{:?}", after.diagnostics);
    assert_eq!(after.hits.len(), before.hits.len());
    for (x, y) in after.hits.iter().zip(before.hits.iter()) {
        assert_eq!(x.doc, y.doc);
        assert!((x.score - y.score).abs() < 1e-5);
    }
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn empty_shard_segment_opens() {
    // Guard for write_cluster's empty-shard path: an empty MemIndex must
    // still produce an openable segment.
    let a = analysis_cfg();
    let dir = tmpdir("empty");
    let idx = MemIndex::new(a);
    assert!(idx.is_empty());
    let meta = write_segment(&dir, &idx, true).unwrap();
    let seg = SegmentReader::open(&dir.join(&meta.file_name)).unwrap();
    assert_eq!(seg.len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
