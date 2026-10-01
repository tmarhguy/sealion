//! Persistent segment round-trips, corruption detection, and publication
//! safety (spec §18–19, §96, §99, §108).
//!
//! - `segment search == memindex search` on identical content.
//! - `merge(segments)`-shape property: one segment per half, unioned search
//!   equals the single-segment result.
//! - Single-bit corruption anywhere is detected on open.
//! - A failed publication never leaves a visible segment.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::mem_index::MemIndex;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;

fn doc(id: u64, title: &str, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic { name: "segments".into() },
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

fn corpus() -> MemIndex {
    let mut idx = MemIndex::new(AnalysisConfig::default());
    idx.add_document(doc(1, "distributed systems", "distributed database systems and consensus"));
    idx.add_document(doc(2, "compiler optimization", "compiler passes and interpreter loops"));
    idx.add_document(doc(3, "database internals", "storage engines and query planning"));
    idx.add_document(doc(17, "network file systems", "replication and partial failure handling"));
    idx
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sealion-seg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn seg_postings(r: &SegmentReader, field: Field, term: &str) -> Vec<(u64, Vec<u32>)> {
    r.postings(field, term).unwrap().into_iter().map(|p| (p.doc.0, p.positions)).collect()
}

fn mem_postings(idx: &MemIndex, field: Field, term: &str) -> Vec<(u64, Vec<u32>)> {
    idx.postings(field, term).iter().map(|p| (p.doc.0, p.positions.clone())).collect()
}

#[test]
fn round_trip_preserves_every_posting_list() {
    let dir = tmpdir("roundtrip");
    let idx = corpus();
    for compress in [true, false] {
        let meta = write_segment(&dir, &idx, compress).unwrap();
        let reader = SegmentReader::open(&dir.join(&meta.file_name)).unwrap();
        assert_eq!(reader.len(), idx.len());
        assert_eq!(reader.dictionary().len(), idx.term_count());
        for (field, term, _) in idx.iter_postings() {
            assert_eq!(
                seg_postings(&reader, field, term),
                mem_postings(&idx, field, term),
                "postings diverged for {field:?}:{term} (compress={compress})"
            );
        }
        // Stored documents come back intact.
        for d in idx.iter_docs() {
            assert_eq!(reader.get(d.id).unwrap().title, d.title);
            assert_eq!(reader.get(d.id).unwrap().body, d.body);
        }
        // Statistics match the source index.
        assert_eq!(reader.stats().doc_count, idx.len() as u64);
        for f in Field::ALL {
            assert_eq!(reader.stats().field_tokens[f.index()], idx.field_token_total(f));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn split_segments_union_to_same_results() {
    // merge(segments) preserves search results (§96): indexing halves
    // separately and unioning per-term postings matches the whole.
    let dir = tmpdir("split");
    let mut left = MemIndex::new(AnalysisConfig::default());
    left.add_document(doc(1, "distributed systems", "distributed database systems"));
    left.add_document(doc(2, "compiler optimization", "compiler passes"));
    let mut right = MemIndex::new(AnalysisConfig::default());
    right.add_document(doc(3, "database internals", "storage engines"));
    right.add_document(doc(17, "network file systems", "replication handling"));

    let m1 = write_segment(&dir, &left, true).unwrap();
    let m2 = write_segment(&dir, &right, true).unwrap();
    let r1 = SegmentReader::open(&dir.join(&m1.file_name)).unwrap();
    let r2 = SegmentReader::open(&dir.join(&m2.file_name)).unwrap();

    let mut whole = MemIndex::new(AnalysisConfig::default());
    for d in left.iter_docs().chain(right.iter_docs()) {
        whole.add_document(d.clone());
    }
    for (field, term, _) in whole.iter_postings() {
        let mut union = seg_postings(&r1, field, term);
        union.extend(seg_postings(&r2, field, term));
        union.sort();
        union.dedup();
        let mut expected = mem_postings(&whole, field, term);
        expected.sort();
        assert_eq!(union, expected, "split diverged for {field:?}:{term}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn single_bit_corruption_is_detected() {
    let dir = tmpdir("corrupt");
    let idx = corpus();
    let meta = write_segment(&dir, &idx, true).unwrap();
    let path = dir.join(&meta.file_name);
    let bytes = std::fs::read(&path).unwrap();
    // Flip one bit in each of several regions; every variant must fail open.
    for flip_at in [10, 60, bytes.len() / 2, bytes.len() - 5] {
        let mut bad = bytes.clone();
        bad[flip_at] ^= 0x01;
        std::fs::write(&path, &bad).unwrap();
        assert!(SegmentReader::open(&path).is_err(), "undetected corruption at {flip_at}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_publication_leaves_no_visible_segment() {
    // Writing into a path that is a file (not a dir) fails; nothing is
    // published and no manifest appears.
    let dir = tmpdir("failpub");
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let idx = corpus();
    assert!(write_segment(&blocker.join("sub"), &idx, true).is_err());
    assert!(Manifest::load_or_new(&blocker.join("sub")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_index_round_trips() {
    let dir = tmpdir("empty");
    let idx = MemIndex::new(AnalysisConfig::default());
    let meta = write_segment(&dir, &idx, true).unwrap();
    let reader = SegmentReader::open(&dir.join(&meta.file_name)).unwrap();
    assert!(reader.is_empty());
    assert_eq!(reader.dictionary().len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
