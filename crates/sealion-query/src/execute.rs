//! Boolean execution over any index generation (spec §33).
//!
//! Set operations run on DocId-sorted posting lists. Intersections use
//! linear two-pointer for similarly sized lists and galloping (exponential)
//! search when one side is ≥8× the other (milestone 08 planner). `And`
//! arms execute cheapest-first by df estimate with early exit; semantics
//! (sorted, deduplicated DocId sets) are unchanged.

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
        Query::Phrase { field, terms } => phrase_docs(view, *field, terms),
        Query::Prefix { field, prefix } => prefix_docs(view, *field, prefix),
        Query::Fuzzy {
            field,
            term,
            distance,
        } => fuzzy_docs(view, *field, term, *distance),
        Query::Site(host) => site_docs(view, host),
        Query::And(children) => {
            // Planner: estimate arm costs, execute cheapest first with
            // early exit. Estimates are df-based (no full evaluation).
            let mut order: Vec<usize> = (0..children.len()).collect();
            order.sort_by_key(|&i| estimate_len(view, &children[i]));
            let mut acc: Option<Vec<DocId>> = None;
            for i in order {
                let list = search_view(view, &children[i])?;
                acc = Some(match acc {
                    None => list,
                    Some(a) => intersect_auto(&a, &list),
                });
                if acc.as_ref().is_some_and(Vec::is_empty) {
                    break;
                }
            }
            Ok(acc.unwrap_or_default())
        }
        Query::Or(children) => {
            let mut acc = Vec::new();
            for q in children {
                acc = union_sorted(&acc, &search_view(view, q)?);
            }
            Ok(acc)
        }
        Query::Not(child) => Ok(difference_sorted(
            &view.all_doc_ids(),
            &search_view(view, child)?,
        )),
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

/// Exact-phrase match over one field: intersect doc sets, then require
/// consecutive positions `p, p+1, …` in term order.
pub fn phrase_docs<V: IndexView>(view: &V, field: Field, terms: &[String]) -> Result<Vec<DocId>> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    // Load posting lists once.
    let mut lists = Vec::with_capacity(terms.len());
    for t in terms {
        lists.push(view.postings(field, t)?);
    }
    if lists.iter().any(Vec::is_empty) {
        return Ok(Vec::new());
    }
    // Candidate docs = intersection of doc sets.
    let mut docs: Vec<DocId> = lists[0].iter().map(|p| p.doc).collect();
    for list in lists.iter().skip(1) {
        let ids: Vec<DocId> = list.iter().map(|p| p.doc).collect();
        docs = intersect_sorted(&docs, &ids);
        if docs.is_empty() {
            return Ok(Vec::new());
        }
    }
    // Positional check per candidate.
    let pos_of = |li: usize, doc: DocId| -> Option<&[u32]> {
        lists[li]
            .iter()
            .find(|p| p.doc == doc)
            .map(|p| p.positions.as_slice())
    };
    let mut out = Vec::new();
    'docs: for doc in docs {
        let first = pos_of(0, doc).expect("candidate in first list");
        for &base in first {
            let mut ok = true;
            for (li, _) in terms.iter().enumerate().skip(1) {
                let plist = pos_of(li, doc).expect("candidate in list");
                // Binary search: positions are sorted.
                if plist.binary_search(&(base + li as u32)).is_err() {
                    ok = false;
                    break;
                }
            }
            if ok {
                out.push(doc);
                continue 'docs;
            }
        }
    }
    Ok(out)
}

/// Estimated result size for planner ordering (df-based, cheap).
/// Overestimates are fine; ordering only affects speed, not results.
fn estimate_len<V: IndexView>(view: &V, query: &Query) -> usize {
    match query {
        Query::Term { field, term } => match field {
            Some(f) => view.postings(*f, term).map(|p| p.len()).unwrap_or(0),
            None => Field::ALL
                .iter()
                .map(|f| view.postings(*f, term).map(|p| p.len()).unwrap_or(0))
                .sum(),
        },
        Query::Phrase { field, terms } => terms
            .iter()
            .map(|t| view.postings(*field, t).map(|p| p.len()).unwrap_or(0))
            .min()
            .unwrap_or(0),
        Query::Prefix { field, prefix } => {
            let fields: &[Field] = match field {
                Some(f) => std::slice::from_ref(f),
                None => &Field::ALL,
            };
            fields
                .iter()
                .map(|f| {
                    view.field_terms(*f)
                        .iter()
                        .filter(|t| t.starts_with(prefix.as_str()))
                        .count()
                        * 2
                })
                .sum()
        }
        Query::Fuzzy { .. } => view.len() / 2,
        Query::Site(_) => view.len() / 4,
        Query::And(children) => children
            .iter()
            .map(|q| estimate_len(view, q))
            .min()
            .unwrap_or(0),
        Query::Or(children) => children.iter().map(|q| estimate_len(view, q)).sum(),
        Query::Not(_) => view.len(),
        Query::MatchAll => view.len(),
        Query::MatchNothing => 0,
    }
}

fn prefix_docs<V: IndexView>(view: &V, field: Option<Field>, prefix: &str) -> Result<Vec<DocId>> {
    let fields: &[Field] = match &field {
        Some(f) => std::slice::from_ref(f),
        None => &Field::ALL,
    };
    let mut acc = Vec::new();
    for f in fields {
        for term in view.field_terms(*f) {
            if term.starts_with(prefix) {
                let ids: Vec<DocId> = view
                    .postings(*f, &term)?
                    .into_iter()
                    .map(|p| p.doc)
                    .collect();
                acc = union_sorted(&acc, &ids);
            }
        }
    }
    Ok(acc)
}

fn fuzzy_docs<V: IndexView>(
    view: &V,
    field: Option<Field>,
    term: &str,
    distance: u8,
) -> Result<Vec<DocId>> {
    let fields: &[Field] = match &field {
        Some(f) => std::slice::from_ref(f),
        None => &Field::ALL,
    };
    let mut acc = Vec::new();
    for f in fields {
        for cand in view.field_terms(*f) {
            if edit_distance_capped(term, &cand, distance) <= distance {
                let ids: Vec<DocId> = view
                    .postings(*f, &cand)?
                    .into_iter()
                    .map(|p| p.doc)
                    .collect();
                acc = union_sorted(&acc, &ids);
            }
        }
    }
    Ok(acc)
}

fn site_docs<V: IndexView>(view: &V, host: &str) -> Result<Vec<DocId>> {
    let needle = host.to_lowercase();
    let mut out: Vec<DocId> = view
        .all_doc_ids()
        .into_iter()
        .filter(|id| {
            view.get(*id)
                .is_some_and(|d| d.url.to_lowercase().contains(&needle))
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Levenshtein distance capped at `cap` (early exit for long diffs).
/// Operates on chars; vocabularies are small enough for the baseline.
pub fn edit_distance_capped(a: &str, b: &str, cap: u8) -> u8 {
    let cap = cap as usize;
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > cap {
        return (cap + 1) as u8;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min((cur[j] + 1).min(prev[j + 1] + 1));
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > cap {
            return (cap + 1) as u8;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[b.len()].min(cap + 1)) as u8
}

/// Intersection with automatic algorithm choice: galloping when skewed
/// (≥8×), linear otherwise. Both exact; benchmark threshold in docs.
pub fn intersect_auto(a: &[DocId], b: &[DocId]) -> Vec<DocId> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if large.len() >= small.len() * 8 {
        intersect_galloping(small, large)
    } else {
        intersect_sorted(a, b)
    }
}

/// Galloping intersection: exponential skip-ahead in `large` then binary
/// search. Wins on skewed sizes; exact (equals [`intersect_sorted`]).
pub fn intersect_galloping(small: &[DocId], large: &[DocId]) -> Vec<DocId> {
    let mut out = Vec::with_capacity(small.len().min(large.len()));
    let mut base = 0usize;
    for s in small {
        if base >= large.len() {
            break;
        }
        // Exponential advance while safely below target.
        let mut step = 1usize;
        while base + step < large.len() && large[base + step] < *s {
            base += step;
            step *= 2;
        }
        match large[base..].binary_search(s) {
            Ok(rel) => {
                out.push(*s);
                base += rel + 1;
            }
            Err(rel) => base += rel,
        }
    }
    out
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

    #[test]
    fn galloping_equals_linear_on_skewed_and_even() {
        // Skewed: 3 vs 200.
        let small: Vec<DocId> = vec![DocId(5), DocId(100), DocId(199)];
        let large: Vec<DocId> = (0..200).map(DocId).collect();
        assert_eq!(
            intersect_galloping(&small, &large),
            intersect_sorted(&small, &large)
        );
        assert_eq!(
            intersect_auto(&small, &large),
            intersect_sorted(&small, &large)
        );
        // Even sizes, empty, disjoint.
        let a: Vec<DocId> = (0..50).map(|i| DocId(i * 2)).collect();
        let b: Vec<DocId> = (0..50).map(|i| DocId(i * 2 + 1)).collect();
        assert!(intersect_galloping(&a, &b).is_empty());
        assert_eq!(intersect_galloping(&a, &a), intersect_sorted(&a, &a));
        assert!(intersect_auto(&[], &b).is_empty());
    }

    #[test]
    fn prefix_fuzzy_site_agree_with_reference() {
        use crate::reference::reference_search;
        let config = cfg();
        let docs = vec![
            doc(1, "compiler design", "compiler optimization passes"),
            doc(2, "interpreter guide", "bytecode interpreter loops"),
            doc(3, "compilation backend", "javascript code generation"),
        ];
        let mut idx = MemIndex::new(config.clone());
        for d in &docs {
            idx.add_document(d.clone());
        }
        let queries = vec![
            Query::Prefix {
                field: Some(Field::Body),
                prefix: "compil".into(),
            },
            Query::Prefix {
                field: None,
                prefix: "compil".into(),
            },
            Query::Fuzzy {
                field: Some(Field::Body),
                term: "compiler".into(),
                distance: 1,
            },
            Query::Fuzzy {
                field: None,
                term: "compilar".into(),
                distance: 1,
            },
            Query::Site("t://1".into()),
            Query::and(vec![
                Query::Prefix {
                    field: None,
                    prefix: "compil".into(),
                },
                Query::negate(Query::term(None, "javascript")),
            ]),
        ];
        for q in &queries {
            assert_eq!(
                search(&idx, q),
                reference_search(&docs, &config, q),
                "divergence on {q:?}"
            );
        }
    }
}
