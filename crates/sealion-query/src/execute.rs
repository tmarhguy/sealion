//! Boolean execution over any index generation (spec §33, subset).
//!
//! Set operations run on DocId-sorted posting lists with linear
//! two-pointer intersection and union. Galloping search and skip-assisted
//! intersection arrive with the query planner in milestone 08; the
//! semantics defined here (sorted, deduplicated DocId sets) stay fixed.

use sealion_core::document::DocId;
use sealion_core::error::Result;
use sealion_core::field::Field;
use sealion_index::mem_index::MemIndex;
use sealion_index::view::IndexView;

use crate::query::Query;

/// Execute a Boolean query against the index. Returns sorted DocIds.
pub fn search(index: &MemIndex, query: &Query) -> Vec<DocId> {
    search_view(index, query).expect("in-memory postings never fail")
}

/// Execute a Boolean query against any index generation (in-memory or
/// persistent segment). Returns sorted DocIds.
pub fn search_view<V: IndexView>(view: &V, query: &Query) -> Result<Vec<DocId>> {
    match query {
        Query::Term { field, term } => term_docs(view, *field, term),
        Query::And(children) => {
            let mut lists: Vec<Vec<DocId>> = Vec::with_capacity(children.len());
            for q in children {
                lists.push(search_view(view, q)?);
            }
            // Cheapest first: intersect smallest lists first.
            lists.sort_by_key(Vec::len);
            let mut acc = lists.first().cloned().unwrap_or_default();
            for list in lists.iter().skip(1) {
                acc = intersect_sorted(&acc, list);
                if acc.is_empty() {
                    break;
                }
            }
            Ok(acc)
        }
        Query::Or(children) => {
            let mut acc = Vec::new();
            for q in children {
                acc = union_sorted(&acc, &search_view(view, q)?);
            }
            Ok(acc)
        }
        Query::Not(child) => {
            Ok(difference_sorted(&view.all_doc_ids(), &search_view(view, child)?))
        }
        Query::MatchAll => Ok(view.all_doc_ids()),
        Query::MatchNothing => Ok(Vec::new()),
    }
}

fn term_docs<V: IndexView>(view: &V, field: Option<Field>, term: &str) -> Result<Vec<DocId>> {
    match field {
        Some(f) => Ok(view.postings(f, term)?.into_iter().map(|p| p.doc).collect()),
        None => {
            let mut acc = Vec::new();
            for f in Field::ALL {
                let list: Vec<DocId> = view.postings(f, term)?.into_iter().map(|p| p.doc).collect();
                acc = union_sorted(&acc, &list);
            }
            Ok(acc)
        }
    }
}

/// Sorted intersection (linear two-pointer).
pub fn intersect_sorted(a: &[DocId], b: &[DocId]) -> Vec<DocId> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Sorted union (deduplicated).
pub fn union_sorted(a: &[DocId], b: &[DocId]) -> Vec<DocId> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Sorted difference `a − b`.
pub fn difference_sorted(a: &[DocId], b: &[DocId]) -> Vec<DocId> {
    let mut out = Vec::with_capacity(a.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::{AnalysisConfig, StemmerKind};
    use sealion_core::document::{Document, Source};

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

    fn index() -> MemIndex {
        let mut idx = MemIndex::new(cfg());
        idx.add_document(doc(1, "compiler design", "compiler optimization passes"));
        idx.add_document(doc(2, "interpreter guide", "bytecode interpreter loops"));
        idx.add_document(doc(3, "compiler backend", "javascript code generation"));
        idx
    }

    fn ids(v: &[DocId]) -> Vec<u64> {
        v.iter().map(|d| d.0).collect()
    }

    #[test]
    fn spec_boolean_examples() {
        let idx = index();
        // compiler AND optimization → doc 1.
        let q = Query::and(vec![
            Query::term(Some(Field::Body), "compiler"),
            Query::term(Some(Field::Body), "optimization"),
        ]);
        assert_eq!(ids(&search(&idx, &q)), vec![1]);
        // compiler OR interpreter → docs 1, 2, 3 (doc 3 title has "compiler").
        let q = Query::or(vec![
            Query::term(None, "compiler"),
            Query::term(None, "interpreter"),
        ]);
        assert_eq!(ids(&search(&idx, &q)), vec![1, 2, 3]);
        // compiler NOT javascript → docs 1 (doc 3 has javascript).
        let q = Query::and(vec![
            Query::term(None, "compiler"),
            Query::negate(Query::term(None, "javascript")),
        ]);
        assert_eq!(ids(&search(&idx, &q)), vec![1]);
    }

    #[test]
    fn field_scoping_title_compiler() {
        let idx = index();
        let q = Query::term(Some(Field::Title), "compiler");
        assert_eq!(ids(&search(&idx, &q)), vec![1, 3]);
        let q = Query::term(Some(Field::Body), "compiler");
        assert_eq!(ids(&search(&idx, &q)), vec![1]);
    }

    #[test]
    fn empty_and_or_identities() {
        let idx = index();
        assert_eq!(search(&idx, &Query::and(vec![])), Vec::new());
        assert_eq!(search(&idx, &Query::or(vec![])), Vec::new());
        assert_eq!(ids(&search(&idx, &Query::MatchAll)), vec![1, 2, 3]);
        assert_eq!(search(&idx, &Query::MatchNothing), Vec::new());
    }

    #[test]
    fn not_is_complement() {
        let idx = index();
        let q = Query::negate(Query::term(None, "compiler"));
        assert_eq!(ids(&search(&idx, &q)), vec![2]);
    }

    #[test]
    fn set_helpers_hold() {
        let a = vec![DocId(1), DocId(3), DocId(5)];
        let b = vec![DocId(2), DocId(3), DocId(4)];
        assert_eq!(intersect_sorted(&a, &b), vec![DocId(3)]);
        assert_eq!(
            union_sorted(&a, &b),
            vec![DocId(1), DocId(2), DocId(3), DocId(4), DocId(5)]
        );
        assert_eq!(difference_sorted(&a, &b), vec![DocId(1), DocId(5)]);
    }
}
