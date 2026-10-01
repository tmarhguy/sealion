//! Correctness properties under randomized updates (§96–97, §108).
//!
//! - `incremental indexing == clean rebuild`: random ADD/UPDATE/DELETE
//!   sequences across segments (with tombstones and shadowing) expose
//!   exactly the live set a fresh single-segment build exposes — document
//!   membership, term postings, and search results.
//! - `merge(segments)` preserves the live set and its postings.
//! - `deleted docs never appear` in any view after any op sequence.
//! - `phrase hits truly contain the phrase` (positional invariant,
//!   oracle-independent).

use sealion_core::config::{AnalysisConfig, StemmerKind};
use sealion_core::document::{DocId, Document, Source};
use sealion_index::mem_index::MemIndex;
use sealion_index::merge::merge_into_memindex;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_index::view::{IndexView, MultiSegmentView};

fn cfg() -> AnalysisConfig {
    AnalysisConfig {
        stop_words: false,
        stemmer: StemmerKind::None,
        ..AnalysisConfig::default()
    }
}

fn doc(id: u64, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic {
            name: "prop".into(),
        },
        url: format!("test://{id}"),
        title: format!("title {id}"),
        headings: Vec::new(),
        body: body.into(),
        anchor_text: Vec::new(),
        metadata: Default::default(),
        timestamp: 0,
        language: "en".into(),
        content_hash: String::new(),
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

fn tmpdir(name: &str, seed: u64) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sealion-prop-{name}-{seed}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn vocab_body(rng: &mut Rng) -> String {
    const V: [&str; 8] = [
        "alpha", "beta", "gamma", "delta", "common", "rare", "term", "word",
    ];
    (0..1 + rng.next(6))
        .map(|_| V[rng.next(8)])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply a random op sequence to segments; mirror it in a clean MemIndex.
/// Returns (dir with segments + tombstones, clean index, deleted set).
fn build_pair(
    seed: u64,
    ops: usize,
) -> (
    std::path::PathBuf,
    Vec<u64>,
    MemIndex,
    std::collections::BTreeSet<u64>,
) {
    let a = cfg();
    let dir = tmpdir("ops", seed);
    let mut rng = Rng(seed ^ 0x9e3779b97f4a7c15);
    let mut clean = MemIndex::new(a.clone());
    let mut tombstones: Vec<u64> = Vec::new();
    let mut deleted = std::collections::BTreeSet::new();
    // Batch ops into segments of ~4 to force multi-generation overlap.
    let mut batch = MemIndex::new(a.clone());
    let mut batch_n = 0;
    let flush = |dir: &std::path::Path, batch: &mut MemIndex, n: &mut usize| {
        if *n > 0 {
            write_segment(dir, batch, true).unwrap();
            *batch = MemIndex::new(cfg());
            *n = 0;
        }
    };
    for _ in 0..ops {
        match rng.next(10) {
            0..=5 => {
                // ADD or UPDATE (same id twice = update).
                let id = rng.next(12) as u64;
                let body = vocab_body(&mut rng);
                batch.add_document(doc(id, &body));
                clean.add_document(doc(id, &body));
                deleted.remove(&id);
                tombstones.retain(|&t| t != id);
                batch_n += 1;
            }
            6..=7 => {
                // DELETE.
                let id = rng.next(12) as u64;
                clean.remove_document(DocId(id));
                deleted.insert(id);
                if !tombstones.contains(&id) {
                    tombstones.push(id);
                }
            }
            _ => {
                // Flush point: publish current batch as a segment.
                flush(&dir, &mut batch, &mut batch_n);
            }
        }
        if batch_n >= 4 {
            flush(&dir, &mut batch, &mut batch_n);
        }
    }
    flush(&dir, &mut batch, &mut batch_n);
    // Persist tombstones like the CLI delete path does.
    if !tombstones.is_empty() {
        let mut m = Manifest::load_or_new(&dir).unwrap();
        for t in &tombstones {
            if !m.deleted.contains(t) {
                m.deleted.push(*t);
            }
        }
        m.generation += 1;
        m.store(&dir).unwrap();
    }
    (dir, tombstones, clean, deleted)
}

fn open_all(dir: &std::path::Path) -> (Vec<SegmentReader>, Vec<u64>) {
    let m = Manifest::load_or_new(dir).unwrap();
    let readers = m
        .segments
        .iter()
        .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
        .collect();
    (readers, m.deleted)
}

#[test]
fn incremental_equals_clean_rebuild_over_random_ops() {
    for seed in [11u64, 22, 33] {
        let (dir, tombstones, clean, deleted) = build_pair(seed, 60);
        let (readers, tomb) = open_all(&dir);
        assert_eq!(tombstones, tomb, "tombstone persistence seed={seed}");
        let view = MultiSegmentView::new(&readers, &tomb);
        // Membership.
        assert_eq!(
            view.all_doc_ids(),
            clean.all_doc_ids(),
            "membership seed={seed}"
        );
        // Deleted docs never appear (in any accessor).
        for d in &deleted {
            assert!(
                view.get(DocId(*d)).is_none(),
                "tombstone leaked seed={seed} doc={d}"
            );
            assert!(!view.all_doc_ids().contains(&DocId(*d)));
        }
        // Postings term-for-term.
        for (field, term, postings) in clean.iter_postings() {
            assert_eq!(
                view.postings(field, term).unwrap(),
                postings,
                "postings {field:?}:{term} seed={seed}"
            );
        }
        // Merged build preserves everything too.
        let merged = merge_into_memindex(&readers, &tomb, &cfg());
        assert_eq!(
            merged.all_doc_ids(),
            clean.all_doc_ids(),
            "merge membership seed={seed}"
        );
        for (field, term, postings) in clean.iter_postings() {
            assert_eq!(
                merged.postings(field, term),
                postings,
                "merge postings seed={seed}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
