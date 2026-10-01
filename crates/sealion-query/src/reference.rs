//! Reference search engine (spec §26): the intentionally slow correctness
//! oracle.
//!
//! For small corpora it scans every document and evaluates the query against
//! re-analyzed field tokens — no inverted index involved. Optimized search
//! must agree with this engine exactly (§41); every search-algorithm change
//! is verified against it (§108, §117).
//!
//! Complexity is deliberately `O(documents × query)`; never use it on a
//! production path.

use std::collections::{BTreeMap, BTreeSet};

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document};
use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;
use sealion_index::mem_index::MemIndex;

use crate::query::Query;

/// Brute-force Boolean search over stored documents.
///
/// Returns sorted DocIds. Must equal [`crate::execute::search`] on the same
/// corpus and query; any divergence is a bug in the indexed path.
pub fn reference_search(docs: &[Document], config: &AnalysisConfig, query: &Query) -> Vec<DocId> {
    let analyzer = Analyzer::new(config);
    // Pre-analyze each doc once into per-field term sets.
    let mut doc_terms: Vec<(DocId, BTreeMap<Field, BTreeSet<String>>)> = Vec::new();
    for doc in docs {
        let mut fields: BTreeMap<Field, BTreeSet<String>> = BTreeMap::new();
        for (field, tokens) in analyzer.analyze_document(doc) {
            fields.insert(field, tokens.into_iter().map(|t| t.term).collect());
        }
        doc_terms.push((doc.id, fields));
    }
    doc_terms.sort_by_key(|(id, _)| *id);
    doc_terms
        .into_iter()
        .filter(|(_, fields)| matches(fields, query))
        .map(|(id, _)| id)
        .collect()
}

/// Reference search directly over an index's stored documents.
pub fn reference_search_index(index: &MemIndex, query: &Query) -> Vec<DocId> {
    let docs: Vec<Document> = index
        .all_doc_ids()
        .into_iter()
        .filter_map(|id| index.get(id).cloned())
        .collect();
    reference_search(&docs, index.config(), query)
}

fn matches(fields: &BTreeMap<Field, BTreeSet<String>>, query: &Query) -> bool {
    match query {
        Query::Term { field, term } => match field {
            Some(f) => fields.get(f).is_some_and(|s| s.contains(term)),
            None => fields.values().any(|s| s.contains(term)),
        },
        Query::And(children) => children.iter().all(|q| matches(fields, q)),
        Query::Or(children) => children.iter().any(|q| matches(fields, q)),
        Query::Not(child) => !matches(fields, child),
        Query::MatchAll => true,
        Query::MatchNothing => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute::search;
    use sealion_core::config::StemmerKind;
    use sealion_core::document::Source;

    fn cfg() -> AnalysisConfig {
        AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        }
    }

    fn doc(id: u64, title: &str, body: &str) -> Document {
        Document {
            id: DocId(id),
            source: Source::Synthetic { name: "t".into() },
            url: format!("t://{id}"),
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
            doc(1, "compiler design", "compiler optimization passes"),
            doc(2, "interpreter guide", "bytecode interpreter loops"),
            doc(3, "compiler backend", "javascript code generation"),
            doc(4, "empty", "the a an of"),
            doc(5, "databases", "distributed database systems"),
        ]
    }

    fn queries() -> Vec<Query> {
        vec![
            Query::term(Some(Field::Body), "compiler"),
            Query::term(None, "compiler"),
            Query::term(Some(Field::Title), "compiler"),
            Query::term(None, "nonexistent"),
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
            Query::negate(Query::term(None, "compiler")),
            Query::MatchAll,
            Query::MatchNothing,
        ]
    }

    #[test]
    fn reference_agrees_with_index_on_hand_cases() {
        let config = cfg();
        let docs = corpus();
        let mut idx = MemIndex::new(config.clone());
        for d in &docs {
            idx.add_document(d.clone());
        }
        for q in queries() {
            assert_eq!(
                search(&idx, &q),
                reference_search(&docs, &config, &q),
                "divergence on {q:?}"
            );
        }
    }

    /// Deterministic pseudo-random agreement fuzz (no new dependencies):
    /// LCG over a small vocabulary, random docs and random Boolean queries.
    #[test]
    fn reference_agrees_on_generated_corpus() {
        let config = AnalysisConfig::default();
        let analyzer = Analyzer::new(&config);
        let vocab = [
            "alpha", "beta", "gamma", "delta", "compiler", "the", "database",
        ];
        let mut rng: u64 = 0x1234_5678_9abc_def1;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as usize
        };

        let mut docs = Vec::new();
        for id in 0..25u64 {
            let mut pick = |n: usize| {
                (0..n)
                    .map(|_| vocab[next() % vocab.len()])
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            docs.push(doc(id, &pick(3), &pick(8)));
        }
        let mut idx = MemIndex::new(config.clone());
        for d in &docs {
            idx.add_document(d.clone());
        }

        for _ in 0..200 {
            let field = if next() % 2 == 0 {
                Some(Field::Body)
            } else {
                None
            };
            let raw = vocab[next() % vocab.len()];
            // Build queries through the same raw-text path users take.
            let term_q = Query::term_raw(&analyzer, field, raw);
            let extra = Query::term_raw(&analyzer, Some(Field::Body), vocab[next() % vocab.len()]);
            let q = match next() % 5 {
                0 => term_q,
                1 => Query::and(vec![term_q, extra]),
                2 => Query::or(vec![term_q, extra]),
                3 => Query::negate(term_q),
                _ => Query::and(vec![term_q, Query::negate(extra)]),
            };
            assert_eq!(
                search(&idx, &q),
                reference_search(&docs, &config, &q),
                "divergence on {q:?}"
            );
        }
    }

    #[test]
    fn deleted_docs_never_appear_in_either_engine() {
        // Correctness property (§96): deleted docs never appear.
        let config = cfg();
        let mut idx = MemIndex::new(config.clone());
        idx.add_document(doc(1, "", "alpha beta"));
        idx.add_document(doc(2, "", "alpha"));
        assert!(idx.remove_document(DocId(1)));
        let q = Query::term(None, "alpha");
        let docs: Vec<Document> = idx
            .all_doc_ids()
            .into_iter()
            .filter_map(|id| idx.get(id).cloned())
            .collect();
        assert_eq!(search(&idx, &q), vec![DocId(2)]);
        assert_eq!(reference_search(&docs, &config, &q), vec![DocId(2)]);
    }
}
