//! In-memory inverted index (spec §14–15).
//!
//! The primary retrieval structure: `term → posting list →
//! (docID, frequency, positions, field)`. Terms are stored per field so
//! ranking can weight fields independently (§29) and queries can scope to
//! one field (`title:compiler`, §30).
//!
//! Posting lists are kept sorted by [`DocId`]; [`MemIndex::add_document`]
//! replaces any previous generation of the same ID (full update/delete
//! semantics with tombstones arrive in milestone 06, §24).
//!
//! Positions use the analysis pipeline's raw-token numbering with gaps for
//! filtered tokens (see [`crate::analysis`]), so phrase queries built on
//! these postings align with query-side analysis.

use std::collections::BTreeMap;

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document};
use sealion_core::field::Field;

use crate::analysis::Analyzer;

/// One term occurrence list inside a single document field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    pub doc: DocId,
    /// Sorted term positions within the field (raw-token numbering).
    pub positions: Vec<u32>,
}

impl Posting {
    /// Number of occurrences in this document field.
    pub fn freq(&self) -> usize {
        self.positions.len()
    }
}

/// In-memory inverted index over analyzed document fields.
#[derive(Debug, Clone)]
pub struct MemIndex {
    config: AnalysisConfig,
    docs: BTreeMap<DocId, Document>,
    /// `(field, term) → postings sorted by DocId`.
    postings: BTreeMap<(Field, String), Vec<Posting>>,
    /// `(doc, field) → number of emitted (kept) tokens`. Raw-token length
    /// (including filtered tokens) is `raw_lengths`; both are kept because
    /// BM25 (milestone 07) needs principled lengths while phrase code needs
    /// raw positions.
    field_lengths: BTreeMap<(DocId, Field), usize>,
}

impl MemIndex {
    pub fn new(config: AnalysisConfig) -> Self {
        Self {
            config,
            docs: BTreeMap::new(),
            postings: BTreeMap::new(),
            field_lengths: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &AnalysisConfig {
        &self.config
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn contains(&self, id: DocId) -> bool {
        self.docs.contains_key(&id)
    }

    pub fn get(&self, id: DocId) -> Option<&Document> {
        self.docs.get(&id)
    }

    /// All document IDs in sorted order.
    pub fn all_doc_ids(&self) -> Vec<DocId> {
        self.docs.keys().copied().collect()
    }

    /// Posting list for one fielded term, sorted by DocId.
    pub fn postings(&self, field: Field, term: &str) -> &[Posting] {
        self.postings
            .get(&(field, term.to_string()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Number of documents containing the fielded term.
    pub fn doc_freq(&self, field: Field, term: &str) -> usize {
        self.postings(field, term).len()
    }

    /// Emitted token count for one document field (0 if unknown).
    pub fn field_length(&self, doc: DocId, field: Field) -> usize {
        self.field_lengths.get(&(doc, field)).copied().unwrap_or(0)
    }

    /// Add a document, replacing any previous generation of the same ID.
    pub fn add_document(&mut self, doc: Document) {
        let id = doc.id;
        if self.docs.contains_key(&id) {
            self.remove_document(id);
        }
        let analyzer = Analyzer::new(&self.config);
        let analyzed = analyzer.analyze_document(&doc);
        // Only touched lists need the sorted-order fix-up (milestone 16:
        // the old loop re-scanned every list per document, O(D×T)).
        let mut touched: Vec<(Field, String)> = Vec::new();
        for (field, tokens) in &analyzed {
            self.field_lengths.insert((id, *field), tokens.len());
            // Group positions per term, preserving sorted order (positions
            // arrive in increasing order from the analyzer).
            let mut by_term: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
            for t in tokens {
                by_term.entry(t.term.as_str()).or_default().push(t.position);
            }
            for (term, positions) in by_term {
                self.postings
                    .entry((*field, term.to_string()))
                    .or_default()
                    .push(Posting { doc: id, positions });
                touched.push((*field, term.to_string()));
            }
        }
        // Posting lists stay sorted: DocIds are pushed in insertion order,
        // which may not be sorted if callers insert out of order.
        // (Bulk ingestion in doc order pays nothing here.)
        for key in touched {
            if let Some(list) = self.postings.get_mut(&key) {
                if list.len() >= 2 {
                    let last = list.len() - 1;
                    if list[last - 1].doc > list[last].doc {
                        list.sort_by_key(|p| p.doc);
                    }
                }
            }
        }
        self.docs.insert(id, doc);
    }

    /// Remove a document and all its postings. Returns true if present.
    pub fn remove_document(&mut self, id: DocId) -> bool {
        if self.docs.remove(&id).is_none() {
            return false;
        }
        for field in Field::ALL {
            self.field_lengths.remove(&(id, field));
        }
        let mut empty_keys = Vec::new();
        for (key, list) in self.postings.iter_mut() {
            if let Some(pos) = list.iter().position(|p| p.doc == id) {
                list.remove(pos);
            }
            if list.is_empty() {
                empty_keys.push(key.clone());
            }
        }
        for key in empty_keys {
            self.postings.remove(&key);
        }
        true
    }

    /// Number of distinct fielded terms in the dictionary.
    pub fn term_count(&self) -> usize {
        self.postings.len()
    }

    /// Iterate all stored documents in DocId order.
    pub fn iter_docs(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }

    /// Iterate `(field, term, postings)` in dictionary sort order.
    pub fn iter_postings(&self) -> impl Iterator<Item = (Field, &str, &[Posting])> {
        self.postings
            .iter()
            .map(|((f, t), p)| (*f, t.as_str(), p.as_slice()))
    }

    /// Total emitted tokens for one field across all documents (BM25 stats).
    pub fn field_token_total(&self, field: Field) -> u64 {
        self.field_lengths
            .iter()
            .filter(|((_, f), _)| *f == field)
            .map(|(_, n)| *n as u64)
            .sum()
    }

    /// Total postings (doc-level entries) across all terms.
    pub fn posting_count(&self) -> usize {
        self.postings.values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::StemmerKind;
    use sealion_core::document::Source;

    fn test_config() -> AnalysisConfig {
        AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        }
    }

    fn doc(id: u64, title: &str, body: &str) -> Document {
        Document {
            id: DocId(id),
            source: Source::Synthetic {
                name: "test".into(),
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

    #[test]
    fn spec_term_posting_shape() {
        // Spec §14: "compiler" in doc 17 title@[2] + body@[41,98], doc 93 body@[7].
        // We emulate with controlled single-field content instead of exact
        // offsets: what matters is (docID, frequency, positions, fields).
        let mut idx = MemIndex::new(test_config());
        idx.add_document(doc(17, "a b compiler", "x compiler y compiler"));
        idx.add_document(doc(93, "nothing here", "z compiler"));
        let title = idx.postings(Field::Title, "compiler");
        assert_eq!(title.len(), 1);
        assert_eq!(title[0].doc, DocId(17));
        assert_eq!(title[0].freq(), 1);
        let body = idx.postings(Field::Body, "compiler");
        assert_eq!(body.len(), 2);
        assert_eq!(body[0].doc, DocId(17));
        assert_eq!(body[0].freq(), 2);
        assert_eq!(body[1].doc, DocId(93));
        assert_eq!(body[1].positions.len(), 1);
    }

    #[test]
    fn spec_positional_example_through_index() {
        let mut idx = MemIndex::new(test_config());
        idx.add_document(doc(1, "", "distributed database systems"));
        let p = idx.postings(Field::Body, "database");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].positions, vec![1]);
    }

    #[test]
    fn postings_stay_sorted_for_out_of_order_inserts() {
        let mut idx = MemIndex::new(test_config());
        idx.add_document(doc(30, "", "common term"));
        idx.add_document(doc(5, "", "common term"));
        idx.add_document(doc(17, "", "common term"));
        let p = idx.postings(Field::Body, "common");
        let ids: Vec<u64> = p.iter().map(|x| x.doc.0).collect();
        assert_eq!(ids, vec![5, 17, 30]);
    }

    #[test]
    fn fields_are_indexed_separately() {
        let mut idx = MemIndex::new(test_config());
        idx.add_document(doc(1, "compiler", "nothing relevant"));
        assert_eq!(idx.doc_freq(Field::Title, "compiler"), 1);
        assert_eq!(idx.doc_freq(Field::Body, "compiler"), 0);
    }

    #[test]
    fn re_adding_same_id_replaces() {
        let mut idx = MemIndex::new(test_config());
        idx.add_document(doc(1, "", "alpha beta"));
        idx.add_document(doc(1, "", "gamma"));
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.doc_freq(Field::Body, "alpha"), 0);
        assert_eq!(idx.doc_freq(Field::Body, "gamma"), 1);
    }

    #[test]
    fn remove_drops_postings() {
        let mut idx = MemIndex::new(test_config());
        idx.add_document(doc(1, "", "alpha beta"));
        idx.add_document(doc(2, "", "alpha"));
        assert!(idx.remove_document(DocId(1)));
        assert!(!idx.contains(DocId(1)));
        let p = idx.postings(Field::Body, "alpha");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].doc, DocId(2));
        assert!(!idx.remove_document(DocId(99)));
    }
}
