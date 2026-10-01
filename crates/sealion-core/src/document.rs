//! Document model (spec §5).
//!
//! A [`Document`] is the atomic unit of indexing and retrieval. Document IDs
//! are stable: once assigned, a document's ID never changes, and updates
//! create a new generation of the same ID (see §24 tombstones).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Stable identifier for a document across index generations.
///
/// The type is opaque on purpose: call sites must not assume the encoding.
/// Internally the first implementation uses a `u64` assigned by the indexer
/// in document-insertion order; future persistent formats may change the
/// representation without touching query code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocId(pub u64);

impl DocId {
    /// The reserved doc ID used as a sentinel (never assigned to a document).
    pub const NONE: DocId = DocId(u64::MAX);
}

/// Where a document came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// A file on local disk (absolute or workspace-relative path).
    File { path: String },
    /// A fetched web URL.
    Web { url: String },
    /// Synthesized or test corpus data (benchmarks, relevance datasets).
    Synthetic { name: String },
}

/// A single searchable document (spec §5).
///
/// Fields mirror the spec's required set. `title`, `headings`, `body`, and
/// `anchor_text` are indexed as separate fields (§15) so ranking can weight
/// them independently (§29).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Stable document ID, assigned at ingestion.
    pub id: DocId,
    /// Origin of the document.
    pub source: Source,
    /// Canonical URL (web) or path (file); the human-facing address.
    pub url: String,
    /// Document title (field `title`).
    pub title: String,
    /// Headings in document order (field `heading`).
    pub headings: Vec<String>,
    /// Main text content (field `body`).
    pub body: String,
    /// Inbound anchor text aggregated from crawled links (field `anchor`).
    pub anchor_text: Vec<String>,
    /// Arbitrary key/value metadata (content type, word count, etc.).
    pub metadata: HashMap<String, String>,
    /// Ingestion timestamp, seconds since Unix epoch.
    pub timestamp: u64,
    /// BCP-47 language tag as detected or configured, e.g. `"en"`.
    pub language: String,
    /// Content hash (e.g. hex of a 64-bit hash) used for exact duplicate
    /// detection (§10). Computed over normalized content.
    pub content_hash: String,
}

impl Document {
    /// Convenience constructor for a minimal plain-text document.
    pub fn new_text(
        id: DocId,
        url: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id,
            source: Source::File { path: url.into() },
            url: String::new(),
            title: title.into(),
            headings: Vec::new(),
            body: body.into(),
            anchor_text: Vec::new(),
            metadata: HashMap::new(),
            timestamp: 0,
            language: "en".to_string(),
            content_hash: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_round_trips_through_json() {
        let doc = Document::new_text(DocId(7), "corpus/a.txt", "Hello", "hello world");
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, DocId(7));
        assert_eq!(back.title, "Hello");
        assert_eq!(back.body, "hello world");
    }

    #[test]
    fn doc_id_none_is_reserved() {
        assert_ne!(DocId::NONE, DocId(0));
    }
}
