//! WAND exactness over persistent segments and generations (§41).
//!
//! Block-Max WAND (and plain WAND) over `SegmentReader` and
//! `MultiSegmentView` (including tombstones + shadowing) must equal
//! exhaustive `ranked_search`: same docs, same scores, same order.

use sealion_core::config::{AnalysisConfig, RankingConfig, StemmerKind};
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::mem_index::MemIndex;
use sealion_index::merge::merge_into_memindex;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_index::view::{IndexView, MultiSegmentView};
use sealion_query::query::Query;
use sealion_query::rank::{ranked_search, RankedHit};
use sealion_query::wand::{block_max_wand_search, wand_search};

fn cfg() -> (AnalysisConfig, RankingConfig) {
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
        source: Source::Synthetic {
            name: "wseg".into(),
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

fn corpus() -> Vec<Document> {
    vec![
        doc(1, "compiler design", "compiler optimization passes passes"),
        doc(2, "interpreter guide", "bytecode interpreter loops"),
        doc(3, "compiler backend", "javascript code generation compiler"),
        doc(4, "database systems", "distributed database systems"),
        doc(5, "compiler compiler", "compiler"),
        doc(6, "networking notes", "packets routing and congestion"),
    ]
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
            Query::term(None, "interpreter"),
        ]),
        Query::and(vec![
            Query::term(None, "compiler"),
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
    ]
}

fn assert_same(a: &[RankedHit], b: &[RankedHit], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: len");
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.doc, y.doc, "{what}: order");
        assert!(
            (x.score - y.score).abs() < 1e-5,
            "{what}: score {}",
            x.doc.0
        );
    }
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sealion-wseg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn wand_matches_exhaustive_on_segments() {
    let (a, r) = cfg();
    let dir = tmpdir("seg");
    let mut idx = MemIndex::new(a.clone());
    for d in corpus() {
        idx.add_document(d);
    }
    let meta = write_segment(&dir, &idx, true).unwrap();
    let seg = SegmentReader::open(&dir.join(&meta.file_name)).unwrap();
    for q in queries() {
        for top_k in [1, 3, 10] {
            let want = ranked_search(&seg, &a, &r, &q, top_k).unwrap();
            let (got, _) = wand_search(&seg, &a, &r, &q, top_k).unwrap();
            assert_same(&want, &got, "wand seg {q:?} k={top_k}");
            let (got_bm, _) = block_max_wand_search(&seg, &a, &r, &q, top_k).unwrap();
            assert_same(&want, &got_bm, "block-max seg {q:?} k={top_k}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wand_matches_exhaustive_across_generations() {
    let (a, r) = cfg();
    let dir = tmpdir("gen");
    // Two segments with an update (doc 1 rewritten) + a tombstone (doc 6).
    let mut i1 = MemIndex::new(a.clone());
    for d in corpus().into_iter().take(4) {
        i1.add_document(d);
    }
    write_segment(&dir, &i1, true).unwrap();
    let mut i2 = MemIndex::new(a.clone());
    i2.add_document(doc(1, "compiler design", "rewritten without the keyword"));
    i2.add_document(doc(5, "compiler compiler", "compiler"));
    i2.add_document(doc(6, "networking notes", "packets routing and congestion"));
    write_segment(&dir, &i2, true).unwrap();
    let mut m = Manifest::load_or_new(&dir).unwrap();
    m.add_tombstone(6);
    m.store(&dir).unwrap();

    let readers: Vec<SegmentReader> = m
        .segments
        .iter()
        .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
        .collect();
    let view = MultiSegmentView::new(&readers, &m.deleted);
    // Sanity: merged clean rebuild agrees with the view on membership.
    let merged = merge_into_memindex(&readers, &m.deleted, &a);
    assert_eq!(merged.len(), view.len());
    for q in queries() {
        let want = ranked_search(&view, &a, &r, &q, 10).unwrap();
        // Tombstoned doc 6 never appears; updated doc 1 scores anew.
        assert!(
            !want.iter().any(|h| h.doc == DocId(6)),
            "tombstone leaked on {q:?}"
        );
        let (got, _) = wand_search(&view, &a, &r, &q, 10).unwrap();
        assert_same(&want, &got, "wand gen {q:?}");
        let (got_bm, _) = block_max_wand_search(&view, &a, &r, &q, 10).unwrap();
        assert_same(&want, &got_bm, "block-max gen {q:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
