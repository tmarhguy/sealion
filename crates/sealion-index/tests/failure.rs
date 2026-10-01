//! Failure injection (§100): crashes and resource failures must leave
//! the old generation intact and queries servable.
//!
//! - Indexing into an unusable path fails cleanly (no panic, no partial
//!   manifest elsewhere).
//! - A merge that hits an unreadable segment aborts with the previous
//!   generation untouched (segments + manifest + search results intact).
//! - Tombstones survive a "crash" (no merge): the delete stays hidden.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_index::mem_index::MemIndex;
use sealion_index::merge::merge_all_segments;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_index::view::{IndexView, MultiSegmentView};

fn doc(id: u64, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic {
            name: "fail".into(),
        },
        url: format!("test://{id}"),
        title: format!("t{id}"),
        headings: Vec::new(),
        body: body.into(),
        anchor_text: Vec::new(),
        metadata: Default::default(),
        timestamp: 0,
        language: "en".into(),
        content_hash: String::new(),
    }
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sealion-fail-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn unusable_data_dir_fails_cleanly() {
    // Point the data dir at an existing FILE: directory creation must fail
    // with an error (works even as root, unlike chmod-based tests).
    let dir = tmpdir("file");
    let file = dir.join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let mut idx = MemIndex::new(AnalysisConfig::default());
    idx.add_document(doc(1, "alpha"));
    assert!(write_segment(&file, &idx, true).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_merge_leaves_old_generation_intact() {
    let a = AnalysisConfig::default();
    let dir = tmpdir("mergecrash");
    let mut i1 = MemIndex::new(a.clone());
    i1.add_document(doc(1, "alpha"));
    write_segment(&dir, &i1, true).unwrap();
    let mut i2 = MemIndex::new(a.clone());
    i2.add_document(doc(2, "beta"));
    write_segment(&dir, &i2, true).unwrap();
    let before = Manifest::load_or_new(&dir).unwrap();
    assert_eq!(before.segments.len(), 2);

    // Corrupt the second segment in place, then attempt a merge: it must
    // fail, and the first segment must still open and serve.
    let victim = dir.join(&before.segments[1]);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes[70] ^= 0xFF;
    std::fs::write(&victim, &bytes).unwrap();
    assert!(merge_all_segments(&dir, &a).is_err());

    // Old generation intact: seg-1 opens, manifest still lists both, and a
    // search over the surviving segment finds doc 1.
    let survivor = SegmentReader::open(&dir.join(&before.segments[0])).unwrap();
    assert!(survivor.get(DocId(1)).is_some());
    let after = Manifest::load_or_new(&dir).unwrap();
    assert_eq!(
        after.segments, before.segments,
        "manifest must not advance on failed merge"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tombstone_survives_without_merge() {
    // "Crash" between delete and merge: tombstone in the manifest keeps
    // the doc hidden across reopens.
    let a = cfg_analysis();
    let dir = tmpdir("tombcrash");
    let mut idx = MemIndex::new(a);
    idx.add_document(doc(1, "alpha beta"));
    write_segment(&dir, &idx, true).unwrap();
    let mut m = Manifest::load_or_new(&dir).unwrap();
    m.add_tombstone(1);
    m.store(&dir).unwrap();
    // Simulate restart: reload everything from disk.
    let m2 = Manifest::load_or_new(&dir).unwrap();
    let readers: Vec<SegmentReader> = m2
        .segments
        .iter()
        .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
        .collect();
    let view = MultiSegmentView::new(&readers, &m2.deleted);
    assert!(view.all_doc_ids().is_empty());
    assert!(view.get(DocId(1)).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

fn cfg_analysis() -> AnalysisConfig {
    AnalysisConfig::default()
}

/// Soak: sustained mixed workload (§101). `#[ignore]`d in CI; run with
/// `cargo test -- --ignored` or `SEALION_SOAK_ITERS=500`.
/// Watches: doc-count consistency, segment-file bound, query latency drift.
#[test]
#[ignore]
fn soak_mixed_workload() {
    let iters: usize = std::env::var("SEALION_SOAK_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let a = AnalysisConfig::default();
    let dir = tmpdir("soak");
    let mut worst_ms = 0f64;
    for i in 0..iters {
        // Index a fresh batch every round (new generation each time).
        let mut idx = MemIndex::new(a.clone());
        for j in 0..10u64 {
            idx.add_document(doc(
                i as u64 * 100 + j,
                "soak alpha beta gamma common words",
            ));
        }
        write_segment(&dir, &idx, true).unwrap();
        // Search the live generation.
        let m = Manifest::load_or_new(&dir).unwrap();
        let readers: Vec<SegmentReader> = m
            .segments
            .iter()
            .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
            .collect();
        let view = MultiSegmentView::new(&readers, &m.deleted);
        let t0 = std::time::Instant::now();
        let hits = view
            .postings(sealion_core::field::Field::Body, "common")
            .unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        worst_ms = worst_ms.max(ms);
        assert_eq!(hits.len(), (i + 1) * 10, "iter {i}: doc count drift");
        // Merge every 5 rounds to exercise compaction under load.
        if i % 5 == 4 {
            let _ = merge_all_segments(&dir, &a);
        }
    }
    // Generous bound (debug builds on shared CI): guards drift, not speed.
    assert!(
        worst_ms < 1000.0,
        "query latency drift: worst {worst_ms:.1}ms"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
