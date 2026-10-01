//! Query AST (spec §30, subset for milestone 03).
//!
//! Boolean queries over fielded terms today; the full surface (phrases,
//! proximity, `site:`, prefixes, fuzz) arrives with the query parser in
//! milestone 08. Terms in this AST are **already normalized**: build them
//! with [`Query::term_raw`] so index and reference engines agree exactly
//! (§31, §41).

use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;

/// A Boolean query over normalized terms (plus phrases, milestone 07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// A single normalized term, optionally scoped to one field.
    /// Unscoped terms match a document if *any* field contains the term.
    Term {
        field: Option<Field>,
        term: String,
    },
    /// An exact phrase over one field: terms must appear consecutively in
    /// order (positions `p, p+1, …`). Built by [`Query::phrase_raw`].
    /// Single-token phrases degrade to [`Query::Term`].
    Phrase {
        field: Field,
        terms: Vec<String>,
    },
    /// Prefix match: any indexed term starting with `prefix` (normalized).
    /// Unscoped scans all fields.
    Prefix {
        field: Option<Field>,
        prefix: String,
    },
    /// Fuzzy match within `distance` edits (1–2). Baseline scans the
    /// vocabulary (milestone 08); BK-tree acceleration in milestone 10.
    Fuzzy {
        field: Option<Field>,
        term: String,
        distance: u8,
    },
    /// URL substring filter (`site:example.com`). Evaluated against stored
    /// documents, composable with Boolean operators.
    Site(String),
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    MatchAll,
    MatchNothing,
}

impl Query {
    /// A pre-normalized term. Callers must guarantee `term` went through
    /// the analysis pipeline (see [`Query::term_raw`]).
    pub fn term(field: Option<Field>, term: impl Into<String>) -> Self {
        Query::Term {
            field,
            term: term.into(),
        }
    }

    /// Normalize raw query text through the same pipeline as indexing and
    /// build the matching Boolean query.
    ///
    /// * Scoped (`Some(field)`): the text analyzed with that field's
    ///   pipeline; multiple tokens become an [`Query::And`].
    /// * Unscoped (`None`): the text analyzed per field; matches if **some**
    ///   field contains all of its analyzed tokens (an [`Query::Or`] over
    ///   per-field conjunctions). This is the only correct semantics when
    ///   per-field analysis configs diverge.
    /// * Text analyzing to zero tokens (only stop words, empty) becomes
    ///   [`Query::MatchNothing`].
    pub fn term_raw(analyzer: &Analyzer<'_>, field: Option<Field>, raw: &str) -> Self {
        match field {
            Some(f) => Self::conjunction(f, analyzer.analyze(f, raw)),
            None => {
                let mut arms = Vec::new();
                for f in Field::ALL {
                    let tokens = analyzer.analyze(f, raw);
                    if tokens.is_empty() {
                        continue;
                    }
                    // One arm per field: correct when per-field analysis
                    // configs diverge (each field matched on its own terms).
                    arms.push(Self::conjunction(f, tokens));
                }
                match arms.len() {
                    0 => Query::MatchNothing,
                    1 => arms.into_iter().next().expect("len == 1"),
                    _ => Query::Or(arms),
                }
            }
        }
    }

    fn conjunction(field: Field, tokens: Vec<sealion_index::analysis::Token>) -> Self {
        let mut terms: Vec<String> = tokens.into_iter().map(|t| t.term).collect();
        terms.sort();
        terms.dedup();
        match terms.len() {
            0 => Query::MatchNothing,
            1 => Query::Term {
                field: Some(field),
                term: terms.into_iter().next().expect("len == 1"),
            },
            _ => Query::And(
                terms
                    .into_iter()
                    .map(|term| Query::Term {
                        field: Some(field),
                        term,
                    })
                    .collect(),
            ),
        }
    }

    /// Build an exact-phrase query from raw text through the field's
    /// pipeline. Term order is preserved (no dedup/sort); phrases that
    /// analyze to 0 tokens become [`Query::MatchNothing`], 1 token degrades
    /// to [`Query::Term`].
    pub fn phrase_raw(analyzer: &Analyzer<'_>, field: Field, raw: &str) -> Self {
        let mut tokens = analyzer.analyze(field, raw);
        tokens.sort_by_key(|t| t.position);
        let terms: Vec<String> = tokens.into_iter().map(|t| t.term).collect();
        match terms.len() {
            0 => Query::MatchNothing,
            1 => Query::Term {
                field: Some(field),
                term: terms.into_iter().next().expect("len == 1"),
            },
            _ => Query::Phrase { field, terms },
        }
    }

    /// All normalized terms in the tree (phrase terms included).
    /// Used by ranking to score candidates. Prefix/fuzzy contribute their
    /// literal text (ranking matches them exactly; expansion-aware scoring
    /// arrives with WAND in milestone 09).
    pub fn terms(&self) -> Vec<(Option<Field>, &str)> {
        match self {
            Query::Term { field, term } => vec![(*field, term.as_str())],
            Query::Phrase { field, terms } => {
                terms.iter().map(|t| (Some(*field), t.as_str())).collect()
            }
            Query::Prefix { field, prefix } => vec![(*field, prefix.as_str())],
            Query::Fuzzy { field, term, .. } => vec![(*field, term.as_str())],
            Query::Site(_) => Vec::new(),
            Query::And(children) | Query::Or(children) => {
                children.iter().flat_map(|q| q.terms()).collect()
            }
            Query::Not(child) => child.terms(),
            Query::MatchAll | Query::MatchNothing => Vec::new(),
        }
    }

    /// Leaf-term count for `max_query_terms` enforcement (§102).
    pub fn leaf_count(&self) -> usize {
        match self {
            Query::Term { .. } | Query::Prefix { .. } | Query::Fuzzy { .. } | Query::Site(_) => 1,
            Query::Phrase { terms, .. } => terms.len().max(1),
            Query::And(children) | Query::Or(children) => {
                children.iter().map(|q| q.leaf_count()).sum()
            }
            Query::Not(child) => child.leaf_count(),
            Query::MatchAll | Query::MatchNothing => 0,
        }
    }

    pub fn and(children: Vec<Query>) -> Self {
        Query::And(children)
    }

    pub fn or(children: Vec<Query>) -> Self {
        Query::Or(children)
    }

    pub fn negate(child: Query) -> Self {
        Query::Not(Box::new(child))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::AnalysisConfig;

    fn cfg() -> AnalysisConfig {
        AnalysisConfig::default()
    }

    #[test]
    fn raw_scoped_term_normalizes_like_index() {
        let c = cfg();
        let a = Analyzer::new(&c);
        // "Databases" must normalize exactly as indexing does (§31).
        let q = Query::term_raw(&a, Some(Field::Body), "Databases");
        let indexed = a.analyze(Field::Body, "Databases");
        assert_eq!(
            q,
            Query::Term {
                field: Some(Field::Body),
                term: indexed[0].term.clone()
            }
        );
    }

    #[test]
    fn stop_word_only_query_matches_nothing() {
        let c = cfg();
        let a = Analyzer::new(&c);
        assert_eq!(
            Query::term_raw(&a, Some(Field::Body), "the"),
            Query::MatchNothing
        );
        assert_eq!(Query::term_raw(&a, None, "the and or"), Query::MatchNothing);
    }

    #[test]
    fn multi_token_scoped_query_is_conjunction() {
        let c = AnalysisConfig {
            stop_words: false,
            stemmer: sealion_core::config::StemmerKind::None,
            ..AnalysisConfig::default()
        };
        let a = Analyzer::new(&c);
        assert_eq!(
            Query::term_raw(&a, Some(Field::Body), "distributed systems"),
            Query::And(vec![
                Query::Term {
                    field: Some(Field::Body),
                    term: "distributed".into()
                },
                Query::Term {
                    field: Some(Field::Body),
                    term: "systems".into()
                },
            ])
        );
    }
}
