//! Query AST (spec §30, subset for milestone 03).
//!
//! Boolean queries over fielded terms today; the full surface (phrases,
//! proximity, `site:`, prefixes, fuzz) arrives with the query parser in
//! milestone 08. Terms in this AST are **already normalized**: build them
//! with [`Query::term_raw`] so index and reference engines agree exactly
//! (§31, §41).

use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;

/// A Boolean query over normalized terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// A single normalized term, optionally scoped to one field.
    /// Unscoped terms match a document if *any* field contains the term.
    Term {
        field: Option<Field>,
        term: String,
    },
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
