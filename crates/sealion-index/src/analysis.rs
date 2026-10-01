//! Text analysis pipeline (spec §12–13).
//!
//! ```text
//! Raw Text → Unicode normalization → Tokenizer → Case normalization
//!     → Stop-word handling → Optional stemming → Terms + positions
//! ```
//!
//! The pipeline is driven by [`AnalysisConfig`]: global knobs in
//! `[analysis]` with per-field overrides in `[analysis.<field>]`
//! (`title`, `heading`, `body`, `anchor`). Indexing and query parsing share
//! this exact pipeline so normalized terms match (§31).
//!
//! ## Positions
//!
//! Positions count raw tokenizer tokens, **including** tokens later removed
//! by stop-word or length filtering. Removed tokens leave gaps, so a phrase
//! query analyzed with the same pipeline still aligns:
//!
//! ```text
//! "the distributed database" → distributed@1 database@2   ("the"@0 removed)
//! ```
//!
//! Skipping positions of removed tokens (dense 0,1,2…) would corrupt phrase
//! adjacency across stop words, so we keep the gaps.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::Document;
use sealion_core::field::Field;

use crate::stemmer::stem;

/// One analyzed term occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Normalized (lowercased, stemmed) term text.
    pub term: String,
    /// Raw-token position in the field (gaps where tokens were filtered).
    pub position: u32,
}

/// Analyzer over a borrowed [`AnalysisConfig`].
#[derive(Debug, Clone, Copy)]
pub struct Analyzer<'a> {
    config: &'a AnalysisConfig,
}

impl<'a> Analyzer<'a> {
    pub fn new(config: &'a AnalysisConfig) -> Self {
        Self { config }
    }

    /// Analyze one field's text into positional terms.
    pub fn analyze(&self, field: Field, text: &str) -> Vec<Token> {
        let stop_words = self.config.stop_words_for(field);
        let stemmer = self.config.stemmer_for(field);
        let min_len = self.config.min_token_len_for(field);

        let mut out = Vec::new();
        for (position, raw) in tokenize(text).enumerate() {
            let position = position as u32;
            // Case normalization: Unicode-aware lowercase.
            let lower = raw.to_lowercase();
            if lower.is_empty() {
                continue;
            }
            // Stop-word handling on the unstemmed lowercase form.
            if stop_words && is_stop_word(&lower) {
                continue;
            }
            // Optional stemming.
            let term = stem(&lower, stemmer);
            // Minimum length applies after stemming.
            if term.chars().count() < min_len {
                continue;
            }
            if term.is_empty() {
                continue;
            }
            out.push(Token { term, position });
        }
        out
    }

    /// Analyze every indexed field of a document.
    ///
    /// Returns `(field, tokens)` pairs in canonical field order. Headings
    /// are concatenated in document order; anchor texts are concatenated in
    /// order received.
    pub fn analyze_document(&self, doc: &Document) -> Vec<(Field, Vec<Token>)> {
        let headings = doc.headings.join("\n");
        let anchors = doc.anchor_text.join("\n");
        [
            (Field::Title, doc.title.as_str()),
            (Field::Heading, headings.as_str()),
            (Field::Body, doc.body.as_str()),
            (Field::Anchor, anchors.as_str()),
        ]
        .into_iter()
        .map(|(field, text)| (field, self.analyze(field, text)))
        .collect()
    }
}

/// Split text into raw tokens: maximal runs of alphanumeric characters.
///
/// Unicode-aware (`char::is_alphanumeric`), so `café` stays one token and
/// CJK text tokenizes per character run. All other characters (whitespace,
/// punctuation, symbols, `_`) are separators.
pub fn tokenize(text: &str) -> impl Iterator<Item = &str> {
    let mut start: Option<usize> = None;
    let mut tokens: Vec<&str> = Vec::new();
    for (i, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            tokens.push(&text[s..i]);
        }
    }
    if let Some(s) = start.take() {
        tokens.push(&text[s..]);
    }
    tokens.into_iter()
}

/// Standard English stop-word list (case-insensitive; input must already be
/// lowercased). Kept as a sorted slice so lookup is a binary search with no
/// allocation and no dependency.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

pub fn is_stop_word(lowercased: &str) -> bool {
    STOP_WORDS.binary_search(&lowercased).is_ok()
}

/// Convenience: analyze with default config (Porter + stop words).
pub fn analyze_default(field: Field, text: &str) -> Vec<Token> {
    let default = AnalysisConfig::default();
    Analyzer::new(&default).analyze(field, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::{Config, StemmerKind};
    use sealion_core::document::DocId;
    use sealion_core::document::Source;

    fn analyzer(cfg: &AnalysisConfig) -> Analyzer<'_> {
        Analyzer::new(cfg)
    }

    #[test]
    fn spec_position_example() {
        // Spec §13: "distributed database systems" → 0, 1, 2.
        let cfg = AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        };
        let tokens = analyzer(&cfg).analyze(Field::Body, "distributed database systems");
        assert_eq!(
            tokens,
            vec![
                Token {
                    term: "distributed".into(),
                    position: 0
                },
                Token {
                    term: "database".into(),
                    position: 1
                },
                Token {
                    term: "systems".into(),
                    position: 2
                },
            ]
        );
    }

    #[test]
    fn tokenizer_splits_punctuation_and_keeps_unicode() {
        assert_eq!(
            tokenize("hello, world! café_au-lait").collect::<Vec<_>>(),
            vec!["hello", "world", "café", "au", "lait"]
        );
        assert_eq!(tokenize("").collect::<Vec<_>>(), Vec::<&str>::new());
        assert_eq!(tokenize("...").collect::<Vec<_>>(), Vec::<&str>::new());
    }

    #[test]
    fn case_normalization_matches_index_and_query() {
        let cfg = AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        };
        let a = analyzer(&cfg).analyze(Field::Body, "Databases");
        let b = analyzer(&cfg).analyze(Field::Body, "databases");
        assert_eq!(a, b);
    }

    #[test]
    fn stop_words_leave_position_gaps() {
        let cfg = AnalysisConfig {
            stop_words: true,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        };
        let tokens = analyzer(&cfg).analyze(Field::Body, "the distributed database");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].term, "distributed");
        assert_eq!(tokens[0].position, 1);
        assert_eq!(tokens[1].position, 2);
    }

    #[test]
    fn stemming_applies_per_config() {
        let cfg = Config::default().analysis;
        let tokens = analyzer(&cfg).analyze(Field::Body, "databases distributed");
        assert_eq!(tokens[0].term, "databas");
        assert_eq!(tokens[1].term, "distribut");
    }

    #[test]
    fn per_field_overrides_apply() {
        let cfg = Config::from_toml(
            "[analysis]\nstop_words = true\nstemmer = \"none\"\n\n[analysis.title]\nstemmer = \"porter\"\n",
        )
        .unwrap();
        let a = Analyzer::new(&cfg.analysis);
        // Body keeps unstemmed form.
        assert_eq!(a.analyze(Field::Body, "databases")[0].term, "databases");
        // Title overrides to Porter.
        assert_eq!(a.analyze(Field::Title, "databases")[0].term, "databas");
    }

    #[test]
    fn min_token_len_filters_after_stemming() {
        let cfg = Config::from_toml("[analysis]\nstop_words = false\nmin_token_len = 3\n").unwrap();
        let a = Analyzer::new(&cfg.analysis);
        let terms: Vec<_> = a
            .analyze(Field::Body, "a be cat xxx")
            .into_iter()
            .map(|t| t.term)
            .collect();
        assert_eq!(terms, vec!["cat", "xxx"]);
    }

    #[test]
    fn analyze_document_covers_all_fields() {
        let cfg = AnalysisConfig::default();
        let doc = Document {
            id: DocId(1),
            source: Source::Synthetic { name: "t".into() },
            url: "test".into(),
            title: "Compiler".into(),
            headings: vec!["Optimization".into()],
            body: "compiler optimization".into(),
            anchor_text: vec!["build tools".into()],
            metadata: Default::default(),
            timestamp: 0,
            language: "en".into(),
            content_hash: String::new(),
        };
        let fields = Analyzer::new(&cfg).analyze_document(&doc);
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].0, Field::Title);
        assert!(!fields[0].1.is_empty());
        assert!(!fields[2].1.is_empty());
    }

    #[test]
    fn query_normalization_uses_same_pipeline() {
        // Spec §31: "Databases" → "database" (Porter gives "databas"; the
        // spec's example assumes a lighter stemmer — what matters is that
        // indexing and query analysis agree exactly).
        let cfg = AnalysisConfig::default();
        let a = Analyzer::new(&cfg);
        let indexed = a.analyze(Field::Body, "Databases");
        let queried = a.analyze(Field::Body, "Databases");
        assert_eq!(indexed, queried);
        assert_eq!(indexed[0].term, "databas");
    }
}
