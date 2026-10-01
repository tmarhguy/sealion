//! Unified read API over index generations (spec §17).
//!
//! [`IndexView`] abstracts posting access so query execution is identical
//! whether the data lives in a [`MemIndex`] or a persistent
//! [`SegmentReader`]. Segment merges and future sharding preserve these
//! semantics: same terms, same sorted DocIds, same results.

use sealion_core::document::{DocId, Document};
use sealion_core::error::Result;
use sealion_core::field::Field;

use crate::mem_index::{MemIndex, Posting};
use crate::segment::reader::SegmentReader;

/// Read-only access to one index generation's postings and stored docs.
pub trait IndexView {
    /// Posting list for one fielded term, sorted by DocId.
    fn postings(&self, field: Field, term: &str) -> Result<Vec<Posting>>;
    /// All document IDs in sorted order.
    fn all_doc_ids(&self) -> Vec<DocId>;
    /// Stored document, if present.
    fn get(&self, id: DocId) -> Option<&Document>;
    /// Number of documents.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl IndexView for MemIndex {
    fn postings(&self, field: Field, term: &str) -> Result<Vec<Posting>> {
        Ok(MemIndex::postings(self, field, term).to_vec())
    }

    fn all_doc_ids(&self) -> Vec<DocId> {
        MemIndex::all_doc_ids(self)
    }

    fn get(&self, id: DocId) -> Option<&Document> {
        MemIndex::get(self, id)
    }

    fn len(&self) -> usize {
        MemIndex::len(self)
    }
}

impl IndexView for SegmentReader {
    fn postings(&self, field: Field, term: &str) -> Result<Vec<Posting>> {
        SegmentReader::postings(self, field, term)
    }

    fn all_doc_ids(&self) -> Vec<DocId> {
        SegmentReader::all_doc_ids(self)
    }

    fn get(&self, id: DocId) -> Option<&Document> {
        SegmentReader::get(self, id)
    }

    fn len(&self) -> usize {
        SegmentReader::len(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::{AnalysisConfig, StemmerKind};
    use sealion_core::document::Source;

    fn index() -> MemIndex {
        let cfg = AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        };
        let mut idx = MemIndex::new(cfg);
        idx.add_document(Document {
            id: DocId(1),
            source: Source::Synthetic { name: "t".into() },
            url: "t://1".into(),
            title: "compiler".into(),
            headings: Vec::new(),
            body: "compiler optimization".into(),
            anchor_text: Vec::new(),
            metadata: Default::default(),
            timestamp: 0,
            language: "en".into(),
            content_hash: String::new(),
        });
        idx
    }

    #[test]
    fn mem_view_matches_direct_access() {
        let idx = index();
        let view = &idx as &dyn IndexView;
        assert_eq!(view.len(), 1);
        assert_eq!(view.all_doc_ids(), vec![DocId(1)]);
        assert_eq!(view.postings(Field::Body, "compiler").unwrap().len(), 1);
        assert!(view.get(DocId(1)).is_some());
        assert!(view.get(DocId(99)).is_none());
    }
}
