//! Typo correction and autocomplete (spec §43–47, milestone 10).
//!
//! - **Candidates**: a BK-tree over the live vocabulary finds all terms
//!   within edit distance N without scanning every term (§44). The tree is
//!   built per call from `IndexView::field_terms` (exact, no stale cache);
//!   persisting it is milestone 16 work.
//! - **Ranking** (§47): corrections by (distance asc, df desc, term asc);
//!   completions by (df desc, term asc). No user tracking.
//! - **Fuzzy queries** (`term~N`) keep working through the milestone 08
//!   path; this module powers suggestions (`did you mean`) and the
//!   `sealion complete` prefix surface.

use std::collections::{BTreeMap, HashMap};

use sealion_core::field::Field;
use sealion_index::view::IndexView;

/// Exact Levenshtein distance (chars). BK-tree navigation needs exact
/// values — the capped variant in `execute` would break pruning.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min((cur[j] + 1).min(prev[j + 1] + 1));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// A BK-tree over one field's vocabulary for sublinear typo lookup.
#[derive(Debug, Default)]
pub struct BkTree {
    terms: Vec<String>,
    /// Node index → (distance → child node index).
    children: Vec<HashMap<usize, usize>>,
    root: Option<usize>,
}

impl BkTree {
    pub fn build<'a>(terms: impl IntoIterator<Item = &'a str>) -> Self {
        let mut tree = BkTree::default();
        for t in terms {
            tree.insert(t);
        }
        tree
    }

    fn insert(&mut self, term: &str) {
        let Some(mut node) = self.root else {
            let id = self.terms.len();
            self.terms.push(term.to_string());
            self.children.push(HashMap::new());
            self.root = Some(id);
            return;
        };
        loop {
            let d = levenshtein(&self.terms[node], term);
            if d == 0 {
                return; // Duplicate.
            }
            let next = self.children[node].get(&d).copied();
            match next {
                Some(child) => node = child,
                None => {
                    let id = self.terms.len();
                    self.terms.push(term.to_string());
                    self.children.push(HashMap::new());
                    self.children[node].insert(d, id);
                    return;
                }
            }
        }
    }

    /// All terms within `max_dist` edits: `(term, distance)`, unsorted.
    pub fn query(&self, term: &str, max_dist: usize) -> Vec<(String, usize)> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let d = levenshtein(&self.terms[node], term);
            if d <= max_dist {
                out.push((self.terms[node].clone(), d));
            }
            // Triangle-inequality prune: children at edge k can only match
            // if |k - d| <= max_dist.
            for (&k, &child) in &self.children[node] {
                if k >= d.saturating_sub(max_dist) && k <= d + max_dist {
                    stack.push(child);
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// A ranked spelling suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub field: Field,
    pub term: String,
    pub distance: usize,
    pub df: usize,
}

/// Suggest corrections for one normalized term: BK-tree candidates ranked
/// by (distance asc, df desc, term asc). The term itself (distance 0) is
/// excluded — it needs no correction.
pub fn suggest<V: IndexView>(
    view: &V,
    field: Option<Field>,
    term: &str,
    max_dist: usize,
    limit: usize,
) -> Vec<Suggestion> {
    let fields: &[Field] = match &field {
        Some(f) => std::slice::from_ref(f),
        None => &Field::ALL,
    };
    let mut out = Vec::new();
    for f in fields {
        let vocab = view.field_terms(*f);
        let tree = BkTree::build(vocab.iter().map(String::as_str));
        for (cand, d) in tree.query(term, max_dist) {
            if d == 0 {
                continue;
            }
            let df = view.postings(*f, &cand).map(|p| p.len()).unwrap_or(0);
            out.push(Suggestion {
                field: *f,
                term: cand,
                distance: d,
                df,
            });
        }
    }
    out.sort_by(|a, b| {
        a.distance
            .cmp(&b.distance)
            .then_with(|| b.df.cmp(&a.df))
            .then_with(|| a.term.cmp(&b.term))
    });
    out.truncate(limit);
    out
}

/// A ranked autocompletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub field: Field,
    pub term: String,
    pub df: usize,
}

/// Complete a normalized prefix: vocabulary matches ranked by (df desc,
/// term asc). Uses the sorted dictionary scan (exact); FST/trie migration
/// is a milestone 16 option if profiling demands it.
pub fn complete<V: IndexView>(
    view: &V,
    field: Option<Field>,
    prefix: &str,
    limit: usize,
) -> Vec<Completion> {
    let fields: &[Field] = match &field {
        Some(f) => std::slice::from_ref(f),
        None => &Field::ALL,
    };
    let mut out = Vec::new();
    for f in fields {
        for cand in view.field_terms(*f) {
            if cand.starts_with(prefix) {
                let df = view.postings(*f, &cand).map(|p| p.len()).unwrap_or(0);
                out.push(Completion {
                    field: *f,
                    term: cand,
                    df,
                });
            }
        }
    }
    out.sort_by(|a, b| b.df.cmp(&a.df).then_with(|| a.term.cmp(&b.term)));
    out.truncate(limit);
    out
}

/// Field-name helper shared with the CLI (`title`/`heading`/`body`/`anchor`).
pub fn field_named(name: &str) -> Option<Field> {
    match name.to_ascii_lowercase().as_str() {
        "title" => Some(Field::Title),
        "heading" | "headings" | "h" => Some(Field::Heading),
        "body" => Some(Field::Body),
        "anchor" => Some(Field::Anchor),
        _ => None,
    }
}

/// Naive vocabulary scan (baseline for BK-tree agreement tests).
pub fn naive_within(vocab: &[String], term: &str, max_dist: usize) -> BTreeMap<String, usize> {
    vocab
        .iter()
        .filter_map(|v| {
            let d = levenshtein(v, term);
            (d <= max_dist).then(|| (v.clone(), d))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::{AnalysisConfig, StemmerKind};
    use sealion_core::document::{DocId, Document, Source};
    use sealion_index::mem_index::MemIndex;

    fn cfg() -> AnalysisConfig {
        AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        }
    }

    fn doc(id: u64, body: &str) -> Document {
        Document {
            id: DocId(id),
            source: Source::Synthetic { name: "s".into() },
            url: format!("t://{id}"),
            title: String::new(),
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
    fn bk_tree_matches_naive_scan_on_generated_vocab() {
        let mut rng: u64 = 0x51ab_3f1c_8d2e_77aa;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as usize
        };
        // Random lowercase words over a small alphabet (dense typo space).
        let vocab: Vec<String> = (0..500)
            .map(|_| {
                (0..5 + next() % 4)
                    .map(|_| (b'a' + (next() % 6) as u8) as char)
                    .collect()
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let tree = BkTree::build(vocab.iter().map(String::as_str));
        assert_eq!(tree.len(), vocab.len());
        for _ in 0..50 {
            let probe: String = (0..6)
                .map(|_| (b'a' + (next() % 6) as u8) as char)
                .collect();
            for max_dist in [1, 2] {
                let got: BTreeMap<String, usize> =
                    tree.query(&probe, max_dist).into_iter().collect();
                let want = naive_within(&vocab, &probe, max_dist);
                // Both sides are maps: compare directly.
                assert_eq!(got, want, "probe={probe} d={max_dist}");
            }
        }
    }

    #[test]
    fn suggestions_rank_distance_then_df() {
        let a = cfg();
        let mut idx = MemIndex::new(a);
        // "compiler" df=3, "compile" df=1, "compiled" df=1.
        idx.add_document(doc(1, "compiler alpha"));
        idx.add_document(doc(2, "compiler beta"));
        idx.add_document(doc(3, "compiler gamma"));
        idx.add_document(doc(4, "compile delta"));
        idx.add_document(doc(5, "compiled epsilon"));
        let s = suggest(&idx, Some(Field::Body), "compilar", 2, 10);
        assert!(!s.is_empty());
        // Within distance 2 of "compilar": compiler(1), compile(2),
        // compiled(2). Distance 1 wins regardless of df.
        assert_eq!(s[0].term, "compiler");
        assert_eq!(s[0].distance, 1);
    }

    #[test]
    fn completions_rank_df_then_term() {
        let a = cfg();
        let mut idx = MemIndex::new(a);
        idx.add_document(doc(1, "compiler alpha"));
        idx.add_document(doc(2, "compiler beta"));
        idx.add_document(doc(3, "compiled gamma"));
        let c = complete(&idx, Some(Field::Body), "comp", 10);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].term, "compiler"); // df=2 first.
        assert_eq!(c[0].df, 2);
        // Latency smoke: full-vocab completion stays interactive.
        let start = std::time::Instant::now();
        let _ = complete(&idx, None, "c", 10);
        assert!(start.elapsed().as_millis() < 500);
    }
}
