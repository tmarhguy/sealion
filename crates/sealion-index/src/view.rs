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

    /// Total emitted tokens for a field across live docs (BM25 avgdl input).
    /// Default scans live docs through `analysis` (always correct);
    /// [`MemIndex`] and [`SegmentReader`] override with stored totals.
    fn field_token_total(
        &self,
        field: Field,
        analysis: &sealion_core::config::AnalysisConfig,
    ) -> u64 {
        let analyzer = crate::analysis::Analyzer::new(analysis);
        let mut total = 0u64;
        for id in self.all_doc_ids() {
            if let Some(doc) = self.get(id) {
                let owned;
                let text = match field {
                    Field::Title => doc.title.as_str(),
                    Field::Heading => {
                        owned = doc.headings.join("\n");
                        &owned
                    }
                    Field::Body => doc.body.as_str(),
                    Field::Anchor => {
                        owned = doc.anchor_text.join("\n");
                        &owned
                    }
                };
                // Re-analysis is deterministic: same count as indexing.
                let n = analyzer.analyze(field, text).len() as u64;
                total += n;
            }
        }
        total
    }

    /// Vocabulary for one field, in sort order. Backs prefix/fuzzy
    /// expansion (milestone 08) and autocomplete (milestone 10).
    fn field_terms(&self, field: Field) -> Vec<String>;

    /// Emitted token count for one document field (BM25 doc-len input).
    /// Default re-analyzes the stored document.
    fn field_length(
        &self,
        doc: DocId,
        field: Field,
        analysis: &sealion_core::config::AnalysisConfig,
    ) -> usize {
        match self.get(doc) {
            None => 0,
            Some(d) => {
                let analyzer = crate::analysis::Analyzer::new(analysis);
                let owned;
                let text = match field {
                    Field::Title => d.title.as_str(),
                    Field::Heading => {
                        owned = d.headings.join("\n");
                        &owned
                    }
                    Field::Body => d.body.as_str(),
                    Field::Anchor => {
                        owned = d.anchor_text.join("\n");
                        &owned
                    }
                };
                analyzer.analyze(field, text).len()
            }
        }
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

    fn field_token_total(
        &self,
        field: Field,
        _analysis: &sealion_core::config::AnalysisConfig,
    ) -> u64 {
        MemIndex::field_token_total(self, field)
    }

    fn field_length(
        &self,
        doc: DocId,
        field: Field,
        _analysis: &sealion_core::config::AnalysisConfig,
    ) -> usize {
        MemIndex::field_length(self, doc, field)
    }

    fn field_terms(&self, field: Field) -> Vec<String> {
        // iter_postings is already in dictionary sort order per BTreeMap.
        let mut terms: Vec<String> = self
            .iter_postings()
            .filter(|(f, _, _)| *f == field)
            .map(|(_, t, _)| t.to_string())
            .collect();
        terms.sort();
        terms.dedup();
        terms
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

    fn field_token_total(
        &self,
        field: Field,
        _analysis: &sealion_core::config::AnalysisConfig,
    ) -> u64 {
        self.stats().field_tokens[field.index()]
    }

    fn field_terms(&self, field: Field) -> Vec<String> {
        self.dictionary()
            .field_terms(field)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn field_length(
        &self,
        doc: DocId,
        field: Field,
        analysis: &sealion_core::config::AnalysisConfig,
    ) -> usize {
        // Memoized at the reader (milestone 16): WAND bound precomputation
        // touches every posting's length, which would otherwise re-analyze
        // each stored document once per term it contains.
        let analyzer = crate::analysis::Analyzer::new(analysis);
        self.memo_field_length(doc, field, |d| field_len_of(&analyzer, d, field))
    }
}

/// Generation-aware view over many segments (spec §24).
///
/// Newer segments (later in `readers`) shadow older ones for the same
/// DocId; tombstoned ids are invisible. This is what makes ADD/UPDATE/
/// DELETE work without a full rebuild: re-indexing a corpus appends a new
/// segment whose versions win until a merge compacts them.
pub struct MultiSegmentView<'a> {
    readers: &'a [SegmentReader],
    tombstones: &'a [u64],
}

impl<'a> MultiSegmentView<'a> {
    pub fn new(readers: &'a [SegmentReader], tombstones: &'a [u64]) -> Self {
        Self {
            readers,
            tombstones,
        }
    }

    fn is_deleted(&self, id: DocId) -> bool {
        self.tombstones.contains(&id.0)
    }

    /// Newest stored document for `id`, or None if deleted/shadowed away.
    fn newest_doc(&self, id: DocId) -> Option<&'a Document> {
        self.owner_reader(id).and_then(|r| r.get(id))
    }

    /// Newest reader owning `id` (shadow-aware), for memoized delegation.
    fn owner_reader(&self, id: DocId) -> Option<&'a SegmentReader> {
        if self.is_deleted(id) {
            return None;
        }
        self.readers.iter().rev().find(|r| r.get(id).is_some())
    }
}

impl IndexView for MultiSegmentView<'_> {
    fn field_token_total(
        &self,
        field: Field,
        analysis: &sealion_core::config::AnalysisConfig,
    ) -> u64 {
        // Sum stored totals, then subtract stale (shadowed) versions which
        // we re-analyze (usually few). Tombstoned docs are subtracted too.
        let mut total: u64 = self
            .readers
            .iter()
            .map(|r| r.stats().field_tokens[field.index()])
            .sum();
        let analyzer = crate::analysis::Analyzer::new(analysis);
        let mut seen = std::collections::BTreeSet::new();
        for r in self.readers.iter().rev() {
            for id in r.all_doc_ids() {
                if !seen.insert(id.0) {
                    // Stale version: remove its contribution.
                    if let Some(doc) = r.get(id) {
                        total = total.saturating_sub(field_len_of(&analyzer, doc, field) as u64);
                    }
                }
            }
        }
        // Subtract tombstoned live contributions (newest version's length).
        for t in self.tombstones {
            let id = DocId(*t);
            for r in self.readers.iter().rev() {
                if let Some(doc) = r.get(id) {
                    total = total.saturating_sub(field_len_of(&analyzer, doc, field) as u64);
                    break;
                }
            }
        }
        total
    }

    fn field_length(
        &self,
        doc: DocId,
        field: Field,
        analysis: &sealion_core::config::AnalysisConfig,
    ) -> usize {
        // Delegate to the owning reader so its length memo absorbs
        // repeated touches (WAND bound precomputation hits every posting).
        match self.owner_reader(doc) {
            None => 0,
            Some(r) => IndexView::field_length(r, doc, field, analysis),
        }
    }

    fn field_terms(&self, field: Field) -> Vec<String> {
        use std::collections::BTreeSet;
        let mut set = BTreeSet::new();
        for r in self.readers {
            for t in r.dictionary().field_terms(field) {
                set.insert(t.to_string());
            }
        }
        set.into_iter().collect()
    }

    fn postings(&self, field: Field, term: &str) -> Result<Vec<Posting>> {
        use std::collections::{BTreeMap, BTreeSet};
        // Newest version wins: a doc is owned by the newest segment that
        // contains it. Older postings for an owned-elsewhere doc are stale
        // and must not resurrect (e.g. updated doc no longer has the term).
        let mut best: BTreeMap<DocId, Posting> = BTreeMap::new();
        let mut shadowed: BTreeSet<DocId> = BTreeSet::new();
        for r in self.readers.iter().rev() {
            for p in r.postings(field, term)? {
                if self.is_deleted(p.doc) || shadowed.contains(&p.doc) {
                    continue;
                }
                best.insert(p.doc, p);
            }
            // Every doc in this segment is now owned (or tombstoned);
            // older segments cannot contribute anything for these ids.
            for id in r.all_doc_ids() {
                shadowed.insert(id);
            }
        }
        // Drop any posting whose doc is tombstoned (belt-and-braces; the
        // loop above already skips them).
        best.retain(|id, _| !self.is_deleted(*id));
        Ok(best.into_values().collect())
    }

    fn all_doc_ids(&self) -> Vec<DocId> {
        use std::collections::BTreeSet;
        let mut ids = BTreeSet::new();
        for r in self.readers.iter() {
            for id in r.all_doc_ids() {
                if !self.is_deleted(id) {
                    ids.insert(id);
                }
            }
        }
        ids.into_iter().collect()
    }

    fn get(&self, id: DocId) -> Option<&Document> {
        self.newest_doc(id)
    }

    fn len(&self) -> usize {
        self.all_doc_ids().len()
    }
}

fn field_len_of(analyzer: &crate::analysis::Analyzer<'_>, doc: &Document, field: Field) -> usize {
    match field {
        Field::Title => analyzer.analyze(field, &doc.title).len(),
        Field::Heading => analyzer.analyze(field, &doc.headings.join("\n")).len(),
        Field::Body => analyzer.analyze(field, &doc.body).len(),
        Field::Anchor => analyzer.analyze(field, &doc.anchor_text.join("\n")).len(),
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
