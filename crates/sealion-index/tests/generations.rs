//! Generations: updates shadow, deletes hide, merge compacts (spec §23–24).
//!
//! - Re-indexing the same DocId in a newer segment shadows the old version
//!   (stale terms do not resurrect).
//! - Tombstoned docs are invisible to the multiview but still on disk until
//!   merge; merge drops them and compacts to one segment.
//! - `merge(segments) == clean rebuild` over the live set.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::mem_index::MemIndex;
use sealion_index::merge::{merge_all_segments, merge_into_memindex};
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_index::view::{IndexView, MultiSegmentView};

fn doc(id: u64, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic { name: "gen".into() },
        url: format!("test://{id}"),
        title: format!("doc {id}"),
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
    let dir = std::env::temp_dir().join(format!("sealion-gen-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn readers_of(dir: &std::path::Path) -> (Vec<SegmentReader>, Vec<u64>) {
    let m = Manifest::load_or_new(dir).unwrap();
    let readers = m
        .segments
        .iter()
        .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
        .collect();
    (readers, m.deleted)
}

#[test]
fn updated_doc_does_not_resurrect_stale_terms() {
    let a = AnalysisConfig::default();
    let dir = tmpdir("shadow");
    let mut i1 = MemIndex::new(a.clone());
    i1.add_document(doc(1, "alpha version one"));
    write_segment(&dir, &i1, true).unwrap();
    let mut i2 = MemIndex::new(a.clone());
    i2.add_document(doc(1, "beta version two"));
    write_segment(&dir, &i2, true).unwrap();

    let (readers, tomb) = readers_of(&dir);
    let view = MultiSegmentView::new(&readers, &tomb);
    // Stale term gone, new term present, one live doc.
    assert!(view.postings(Field::Body, "alpha").unwrap().is_empty());
    assert_eq!(view.postings(Field::Body, "beta").unwrap().len(), 1);
    assert_eq!(view.all_doc_ids(), vec![DocId(1)]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tombstone_hides_then_merge_drops() {
    let a = AnalysisConfig::default();
    let dir = tmpdir("tombmerge");
    let mut i1 = MemIndex::new(a.clone());
    i1.add_document(doc(1, "alpha"));
    i1.add_document(doc(2, "beta"));
    write_segment(&dir, &i1, true).unwrap();

    let mut m = Manifest::load_or_new(&dir).unwrap();
    m.add_tombstone(1);
    m.store(&dir).unwrap();

    let (readers, tomb) = readers_of(&dir);
    let view = MultiSegmentView::new(&readers, &tomb);
    assert_eq!(view.all_doc_ids(), vec![DocId(2)]);
    assert!(view.get(DocId(1)).is_none());

    let name = merge_all_segments(&dir, &a).unwrap();
    let after = Manifest::load_or_new(&dir).unwrap();
    assert_eq!(after.segments, vec![name.clone()]);
    assert!(after.deleted.is_empty());
    let r = SegmentReader::open(&dir.join(&name)).unwrap();
    assert_eq!(r.len(), 1);
    assert!(r.get(DocId(1)).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_equals_clean_rebuild_over_live_set() {
    let a = AnalysisConfig::default();
    let dir = tmpdir("equiv");
    let mut i1 = MemIndex::new(a.clone());
    i1.add_document(doc(1, "alpha"));
    i1.add_document(doc(2, "beta gamma"));
    write_segment(&dir, &i1, true).unwrap();
    let mut i2 = MemIndex::new(a.clone());
    i2.add_document(doc(2, "delta updated"));
    i2.add_document(doc(3, "epsilon"));
    write_segment(&dir, &i2, true).unwrap();

    let (readers, tomb) = readers_of(&dir);
    let merged = merge_into_memindex(&readers, &tomb, &a);
    // Clean rebuild of the live set must agree term-for-term.
    let mut clean = MemIndex::new(a.clone());
    clean.add_document(doc(1, "alpha"));
    clean.add_document(doc(2, "delta updated"));
    clean.add_document(doc(3, "epsilon"));
    for (field, term, postings) in clean.iter_postings() {
        assert_eq!(merged.postings(field, term), postings, "{field:?}:{term}");
    }
    assert_eq!(merged.len(), clean.len());
    let _ = std::fs::remove_dir_all(&dir);
}
