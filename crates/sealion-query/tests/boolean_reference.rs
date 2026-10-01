//! End-to-end Boolean search agreement (spec §41, §96, §108).
//!
//! Builds a small corpus through the public crate APIs only, runs queries
//! through the user-facing raw-text path, and asserts indexed execution
//! equals the reference oracle on every query.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;
use sealion_index::mem_index::MemIndex;
use sealion_query::execute::search;
use sealion_query::query::Query;
use sealion_query::reference::{reference_search, reference_search_index};

fn doc(id: u64, title: &str, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic {
            name: "integration".into(),
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
        doc(
            1,
            "distributed systems",
            "distributed database systems and consensus",
        ),
        doc(
            2,
            "compiler optimization",
            "compiler passes and interpreter loops",
        ),
        doc(
            3,
            "database internals",
            "storage engines and query planning",
        ),
        doc(4, "notes", "the a an of nothing searchable here"),
    ]
}

#[test]
fn indexed_search_matches_reference_on_every_query_shape() {
    let config = AnalysisConfig::default();
    let analyzer = Analyzer::new(&config);
    let docs = corpus();
    let mut index = MemIndex::new(config.clone());
    for d in &docs {
        index.add_document(d.clone());
    }

    let raw_queries: Vec<(Option<Field>, &str)> = vec![
        (None, "distributed"),
        (None, "compiler"),
        (Some(Field::Title), "compiler"),
        (Some(Field::Body), "database"),
        (None, "distributed database"),
        (None, "compiler optimization"),
        (None, "the"),
        (None, "nonexistent"),
    ];
    for (field, raw) in raw_queries {
        let q = Query::term_raw(&analyzer, field, raw);
        assert_eq!(
            search(&index, &q),
            reference_search(&docs, &config, &q),
            "divergence on raw query {raw:?}"
        );
        assert_eq!(
            search(&index, &q),
            reference_search_index(&index, &q),
            "index-doc divergence on raw query {raw:?}"
        );
    }

    // Boolean combinations over raw terms.
    let compiled = Query::term_raw(&analyzer, None, "compiler");
    let interpreted = Query::term_raw(&analyzer, None, "interpreter");
    for q in [
        Query::and(vec![compiled.clone(), interpreted.clone()]),
        Query::or(vec![compiled.clone(), interpreted.clone()]),
        Query::negate(compiled.clone()),
        Query::and(vec![compiled.clone(), Query::negate(interpreted.clone())]),
        Query::MatchAll,
        Query::MatchNothing,
    ] {
        assert_eq!(
            search(&index, &q),
            reference_search(&docs, &config, &q),
            "divergence on {q:?}"
        );
    }
}

#[test]
fn deleted_documents_never_appear() {
    let config = AnalysisConfig::default();
    let mut index = MemIndex::new(config.clone());
    for d in corpus() {
        index.add_document(d);
    }
    assert!(index.remove_document(DocId(1)));
    let analyzer = Analyzer::new(&config);
    let q = Query::term_raw(&analyzer, None, "distributed");
    let hits = search(&index, &q);
    assert!(!hits.contains(&DocId(1)));
    let docs: Vec<Document> = index
        .all_doc_ids()
        .into_iter()
        .filter_map(|id| index.get(id).cloned())
        .collect();
    assert_eq!(hits, reference_search(&docs, &config, &q));
}

#[test]
fn incremental_indexing_matches_clean_rebuild() {
    // Correctness property (§96): incremental indexing == clean rebuild.
    let config = AnalysisConfig::default();
    let docs = corpus();
    let mut incremental = MemIndex::new(config.clone());
    for d in &docs {
        incremental.add_document(d.clone());
    }
    incremental.remove_document(DocId(4));
    let replacement = doc(2, "compiler optimization v2", "compiler passes revised");
    incremental.add_document(replacement.clone());

    let mut rebuilt_docs: Vec<Document> = docs
        .into_iter()
        .filter(|d| d.id != DocId(4) && d.id != DocId(2))
        .collect();
    rebuilt_docs.push(replacement);
    let mut rebuilt = MemIndex::new(config.clone());
    for d in &rebuilt_docs {
        rebuilt.add_document(d.clone());
    }

    let analyzer = Analyzer::new(&config);
    for raw in ["compiler", "distributed", "database", "nothing"] {
        let q = Query::term_raw(&analyzer, None, raw);
        assert_eq!(
            search(&incremental, &q),
            search(&rebuilt, &q),
            "incremental != rebuild on {raw:?}"
        );
    }
}
