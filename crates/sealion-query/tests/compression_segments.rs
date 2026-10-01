//! Segment-level compression: size and query-equivalence (spec §89).
//!
//! Writes the same corpus as raw-u64 and delta+varint segments, reports
//! both sizes, and asserts identical search results across layouts.
//! Lives in this crate because it needs both `sealion-index` (segments)
//! and `sealion-query` (search).

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;
use sealion_index::mem_index::MemIndex;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_query::execute::{search, search_view};
use sealion_query::query::Query;

/// Deterministic LCG (no dev-dependencies for benchmarking).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn big_corpus(n_docs: usize) -> MemIndex {
    let mut rng = Rng(12345);
    let vocab: Vec<String> = (0..2000).map(|i| format!("term{i:04}")).collect();
    let mut idx = MemIndex::new(AnalysisConfig::default());
    for id in 0..n_docs as u64 {
        let n_terms = 20 + rng.next() % 60;
        let mut words = Vec::new();
        for _ in 0..n_terms {
            words.push(vocab[(rng.next() as usize) % vocab.len()].clone());
        }
        // Sprinkle a common term and a rare term for the query mix.
        if id % 3 == 0 {
            words.push("common".to_string());
        }
        if id == 7 {
            words.push("rarequark".to_string());
        }
        let body = words.join(" ");
        idx.add_document(Document {
            id: DocId(id),
            source: Source::Synthetic { name: "bench".into() },
            url: format!("bench://{id}"),
            title: format!("doc {id}"),
            headings: Vec::new(),
            body,
            anchor_text: Vec::new(),
            metadata: Default::default(),
            timestamp: 0,
            language: "en".into(),
            content_hash: String::new(),
        });
    }
    idx
}

#[test]
fn segment_compression_report() {
    let dir = std::env::temp_dir().join(format!("sealion-comp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let idx = big_corpus(2000);
    let raw = write_segment(&dir, &idx, false).unwrap();
    let comp = write_segment(&dir, &idx, true).unwrap();
    let raw_bytes = std::fs::metadata(dir.join(&raw.file_name)).unwrap().len();
    let comp_bytes = std::fs::metadata(dir.join(&comp.file_name)).unwrap().len();

    let r_raw = SegmentReader::open(&dir.join(&raw.file_name)).unwrap();
    let r_comp = SegmentReader::open(&dir.join(&comp.file_name)).unwrap();

    // Query-equivalence across layouts on a mixed query set.
    let analyzer = Analyzer::new(idx.config());
    let queries = [
        Query::term_raw(&analyzer, None, "rarequark"),
        Query::term_raw(&analyzer, None, "common"),
        Query::term_raw(&analyzer, Some(Field::Title), "doc"),
        Query::and(vec![
            Query::term_raw(&analyzer, None, "common"),
            Query::term_raw(&analyzer, None, "term0042"),
        ]),
    ];
    for q in &queries {
        let mem = search(&idx, q);
        assert_eq!(search_view(&r_raw, q).unwrap(), mem, "raw layout diverged on {q:?}");
        assert_eq!(search_view(&r_comp, q).unwrap(), mem, "compressed diverged on {q:?}");
    }

    let postings: usize = idx.iter_postings().map(|(_, _, p)| p.len()).sum();
    println!("\nsegment compression (2000 docs):");
    println!("  raw segment:        {raw_bytes:>10} bytes");
    println!(
        "  compressed segment: {comp_bytes:>10} bytes ({:.1}% of raw)",
        100.0 * comp_bytes as f64 / raw_bytes as f64
    );
    println!("  total postings:     {postings:>10}");
    assert!(comp_bytes < raw_bytes, "compressed segment not smaller");

    let _ = std::fs::remove_dir_all(&dir);
}
