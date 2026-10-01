//! Segment-backed search agreement (spec §41).
//!
//! Boolean queries executed against persistent segments must return exactly
//! what the in-memory index (and therefore the reference oracle) returns.
//! Covers compressed and raw layouts, plus multi-segment unions.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;
use sealion_index::mem_index::MemIndex;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_query::execute::{search, search_view};
use sealion_query::query::Query;

fn doc(id: u64, title: &str, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic {
            name: "segsearch".into(),
        },
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

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sealion-segq-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn queries(analyzer: &Analyzer<'_>) -> Vec<Query> {
    let compiler = Query::term_raw(analyzer, None, "compiler");
    let optimization = Query::term_raw(analyzer, Some(Field::Body), "optimization");
    vec![
        compiler.clone(),
        Query::term_raw(analyzer, Some(Field::Title), "compiler"),
        Query::term_raw(analyzer, None, "distributed database"),
        Query::term_raw(analyzer, None, "the"),
        Query::and(vec![compiler.clone(), optimization.clone()]),
        Query::or(vec![compiler.clone(), optimization.clone()]),
        Query::negate(compiler.clone()),
        Query::MatchAll,
        Query::MatchNothing,
    ]
}

#[test]
fn segment_search_matches_memory_on_both_layouts() {
    let config = AnalysisConfig::default();
    let analyzer = Analyzer::new(&config);
    let mut idx = MemIndex::new(config.clone());
    idx.add_document(doc(
        1,
        "distributed systems",
        "distributed database systems",
    ));
    idx.add_document(doc(
        2,
        "compiler optimization",
        "compiler passes and interpreter loops",
    ));
    idx.add_document(doc(
        3,
        "database internals",
        "storage engines and query planning",
    ));

    for compress in [true, false] {
        let dir = tmpdir(if compress { "c" } else { "r" });
        let meta = write_segment(&dir, &idx, compress).unwrap();
        let seg = SegmentReader::open(&dir.join(&meta.file_name)).unwrap();
        for q in queries(&analyzer) {
            assert_eq!(
                search_view(&seg, &q).unwrap(),
                search(&idx, &q),
                "segment diverged on {q:?} (compress={compress})"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn multi_segment_union_matches_single_index() {
    let config = AnalysisConfig::default();
    let analyzer = Analyzer::new(&config);
    let dir = tmpdir("multi");
    let mut whole = MemIndex::new(config.clone());
    let mut readers = Vec::new();
    for d in [
        doc(1, "distributed systems", "distributed database systems"),
        doc(2, "compiler optimization", "compiler passes"),
        doc(3, "database internals", "storage engines"),
    ] {
        let mut one = MemIndex::new(config.clone());
        one.add_document(d.clone());
        whole.add_document(d);
        let meta = write_segment(&dir, &one, true).unwrap();
        readers.push(SegmentReader::open(&dir.join(&meta.file_name)).unwrap());
    }
    for q in queries(&analyzer) {
        let mut union = Vec::new();
        for r in &readers {
            union = sealion_query::execute::union_sorted(&union, &search_view(r, &q).unwrap());
        }
        assert_eq!(union, search(&whole, &q), "multi-segment diverged on {q:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
