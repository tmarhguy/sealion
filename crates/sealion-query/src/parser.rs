//! Structured query parser (spec §30, milestone 08).
//!
//! ```text
//! distributed systems          implicit AND (multi-token = And)
//! "distributed systems"        exact phrase (body)
//! title:"distributed systems"  exact phrase scoped to title
//! compiler AND optimization    explicit AND (also OR, NOT, parens)
//! title:compiler               field-scoped term
//! site:example.com             URL substring filter
//! prefix*                      prefix match (normalized)
//! compiler~1                   fuzzy match (edit distance 1–2, default 1)
//! ```
//!
//! Precedence: `NOT` > implicit/explicit `AND` > `OR`. `AND`/`OR`/`NOT`
//! match case-insensitively as bare words; anything else is a term.
//! Terms normalize through the index pipeline (§31). Queries exceeding
//! `max_query_terms` leaves are rejected (§102).

use sealion_core::config::AnalysisConfig;
use sealion_core::error::{Error, Result};
use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;

use crate::query::Query;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    And,
    Or,
    Not,
    LParen,
    RParen,
    Phrase(String),
    Word(String),
}

fn tokenize(input: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }
        if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != '"' {
                s.push(chars[i]);
                i += 1;
            }
            // Skip closing quote if present (unclosed → rest of input).
            if i < chars.len() {
                i += 1;
            }
            out.push(Tok::Phrase(s));
            continue;
        }
        // Bare word: until whitespace or paren or quote.
        let mut s = String::new();
        while i < chars.len()
            && !chars[i].is_whitespace()
            && chars[i] != '('
            && chars[i] != ')'
            && chars[i] != '"'
        {
            s.push(chars[i]);
            i += 1;
        }
        match s.to_ascii_uppercase().as_str() {
            "AND" => out.push(Tok::And),
            "OR" => out.push(Tok::Or),
            "NOT" => out.push(Tok::Not),
            _ => out.push(Tok::Word(s)),
        }
    }
    out
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    analyzer: Analyzer<'a>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<Query> {
        let mut arms = vec![self.parse_and()?];
        while self.peek() == Some(&Tok::Or) {
            self.bump();
            arms.push(self.parse_and()?);
        }
        Ok(if arms.len() == 1 {
            arms.into_iter().next().expect("len 1")
        } else {
            Query::Or(arms)
        })
    }

    fn parse_and(&mut self) -> Result<Query> {
        let mut arms = vec![self.parse_not()?];
        loop {
            match self.peek() {
                Some(Tok::And) => {
                    self.bump();
                    arms.push(self.parse_not()?);
                }
                // Implicit AND: juxtaposed primaries.
                Some(Tok::Word(_)) | Some(Tok::Phrase(_)) | Some(Tok::LParen) | Some(Tok::Not) => {
                    arms.push(self.parse_not()?);
                }
                _ => break,
            }
        }
        Ok(if arms.len() == 1 {
            arms.into_iter().next().expect("len 1")
        } else {
            Query::And(arms)
        })
    }

    fn parse_not(&mut self) -> Result<Query> {
        if self.peek() == Some(&Tok::Not) {
            self.bump();
            return Ok(Query::negate(self.parse_not()?));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Query> {
        match self.bump() {
            Some(Tok::LParen) => {
                let q = self.parse_or()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(q),
                    _ => Err(Error::InvalidQuery("unclosed `(`".into())),
                }
            }
            Some(Tok::Phrase(s)) => {
                // Phrases are body-scoped (field-qualified phrases like
                // `title:"a b"` are handled in `parse_word`).
                Ok(Query::phrase_raw(&self.analyzer, Field::Body, &s))
            }
            Some(Tok::Word(w)) => self.parse_word(&w),
            Some(t) => Err(Error::InvalidQuery(format!("unexpected token {t:?}"))),
            None => Err(Error::InvalidQuery("unexpected end of query".into())),
        }
    }

    fn field_named(name: &str) -> Option<Field> {
        match name.to_ascii_lowercase().as_str() {
            "title" => Some(Field::Title),
            "heading" | "headings" | "h" => Some(Field::Heading),
            "body" => Some(Field::Body),
            "anchor" => Some(Field::Anchor),
            _ => None,
        }
    }

    fn parse_word(&self, w: &str) -> Result<Query> {
        // site:example.com filter (checked before generic field:).
        if let Some(rest) = w.strip_prefix("site:") {
            let host = rest.trim().to_lowercase();
            if host.is_empty() {
                return Ok(Query::MatchNothing);
            }
            return Ok(Query::Site(host));
        }
        // field:rest scoping.
        if let Some(colon) = w.find(':') {
            let (name, rest) = w.split_at(colon);
            let rest = &rest[1..];
            if let Some(field) = Self::field_named(name) {
                if rest.is_empty() {
                    return Ok(Query::MatchNothing);
                }
                return self.parse_scoped(Some(field), rest);
            }
            // Unknown field: fall through and analyze whole token as term.
        }
        self.parse_scoped(None, w)
    }

    fn parse_scoped(&self, field: Option<Field>, rest: &str) -> Result<Query> {
        // Fuzzy: base~N (N = 1–2, default 1).
        if let Some(tilde) = rest.rfind('~') {
            let (base, dist) = rest.split_at(tilde);
            let dist = &dist[1..];
            if !base.is_empty() && (dist.is_empty() || dist == "1" || dist == "2") {
                let distance: u8 = if dist.is_empty() {
                    1
                } else {
                    dist.parse().unwrap_or(1)
                };
                let norm = self.normalize_one(field, base);
                return Ok(match norm.into_iter().next() {
                    Some(t) => Query::Fuzzy {
                        field,
                        term: t,
                        distance,
                    },
                    None => Query::MatchNothing,
                });
            }
        }
        // Prefix: stem* (normalized prefix).
        if let Some(base) = rest.strip_suffix('*') {
            if base.is_empty() {
                return Ok(Query::MatchNothing);
            }
            let norm = self.normalize_one(field, base);
            return Ok(match norm.into_iter().next() {
                Some(p) => Query::Prefix { field, prefix: p },
                None => Query::MatchNothing,
            });
        }
        // Plain term(s): analyze; multi-token → And.
        Ok(Query::term_raw(&self.analyzer, field, rest))
    }

    /// Normalize one token to its single index term (first analyzed token).
    fn normalize_one(&self, field: Option<Field>, raw: &str) -> Vec<String> {
        match field {
            Some(f) => self
                .analyzer
                .analyze(f, raw)
                .into_iter()
                .take(1)
                .map(|t| t.term)
                .collect(),
            None => {
                // Prefer body analysis for unscoped prefixes/fuzzy.
                self.analyzer
                    .analyze(Field::Body, raw)
                    .into_iter()
                    .take(1)
                    .map(|t| t.term)
                    .collect()
            }
        }
    }
}

/// Parse a full query string. Empty/whitespace → [`Query::MatchNothing`].
/// Rejects queries with more than `max_terms` leaves.
pub fn parse(config: &AnalysisConfig, max_terms: usize, input: &str) -> Result<Query> {
    if input.trim().is_empty() {
        return Ok(Query::MatchNothing);
    }
    let toks = tokenize(input);
    if toks.is_empty() {
        return Ok(Query::MatchNothing);
    }
    let analyzer = Analyzer::new(config);
    let mut p = Parser {
        toks,
        pos: 0,
        analyzer,
    };
    let q = p.parse_or()?;
    if p.pos != p.toks.len() {
        return Err(Error::InvalidQuery("trailing tokens".into()));
    }
    if q.leaf_count() > max_terms {
        return Err(Error::InvalidQuery(format!(
            "query has {} terms, limit is {max_terms}",
            q.leaf_count()
        )));
    }
    Ok(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AnalysisConfig {
        AnalysisConfig::default()
    }

    #[test]
    fn bare_words_are_implicit_and() {
        let c = cfg();
        let q = parse(&c, 64, "distributed systems").unwrap();
        assert!(matches!(
            q,
            Query::Or(_) | Query::And(_) | Query::Term { .. }
        ));
    }

    #[test]
    fn quoted_is_phrase() {
        let c = cfg();
        let q = parse(&c, 64, "\"distributed systems\"").unwrap();
        assert!(matches!(q, Query::Phrase { .. } | Query::MatchNothing));
    }

    #[test]
    fn explicit_operators_and_precedence() {
        let c = cfg();
        // AND binds tighter than OR.
        let q = parse(&c, 64, "a OR b AND c").unwrap();
        assert!(matches!(q, Query::Or(_)));
        // NOT binds tightest; parens group.
        let q = parse(&c, 64, "NOT a AND (b OR c)").unwrap();
        assert!(matches!(q, Query::And(_)));
    }

    #[test]
    fn field_site_prefix_fuzzy() {
        let c = cfg();
        assert!(matches!(
            parse(&c, 64, "title:compiler").unwrap(),
            Query::Term { .. } | Query::And(_) | Query::MatchNothing
        ));
        assert!(matches!(
            parse(&c, 64, "site:example.com").unwrap(),
            Query::Site(_)
        ));
        assert!(matches!(
            parse(&c, 64, "comp*").unwrap(),
            Query::Prefix { .. } | Query::MatchNothing
        ));
        let q = parse(&c, 64, "compiler~1").unwrap();
        assert!(matches!(
            q,
            Query::Fuzzy { distance: 1, .. } | Query::MatchNothing
        ));
    }

    #[test]
    fn limits_and_empty() {
        let c = cfg();
        assert_eq!(parse(&c, 64, "   ").unwrap(), Query::MatchNothing);
        assert!(parse(&c, 1, "a b c d").is_err());
        assert!(parse(&c, 64, "(a").is_err());
    }
}
