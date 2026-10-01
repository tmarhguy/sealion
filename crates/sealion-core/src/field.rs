//! Indexed field taxonomy (spec §15).
//!
//! Each field is indexed separately so ranking can weight them
//! independently (§29). The enum order is the canonical field order used in
//! any per-field arrays.

use serde::{Deserialize, Serialize};

/// The fields SeaLion indexes separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Field {
    /// Document title.
    Title,
    /// Headings (h1..h6), concatenated in document order.
    Heading,
    /// Main body text.
    Body,
    /// Aggregated inbound anchor text from crawled links.
    Anchor,
}

impl Field {
    /// All fields in canonical order.
    pub const ALL: [Field; 4] = [Field::Title, Field::Heading, Field::Body, Field::Anchor];

    /// Index of this field in canonical order.
    pub fn index(self) -> usize {
        match self {
            Field::Title => 0,
            Field::Heading => 1,
            Field::Body => 2,
            Field::Anchor => 3,
        }
    }

    /// Field name as used in query syntax (`title:compiler`).
    pub fn name(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Heading => "heading",
            Field::Body => "body",
            Field::Anchor => "anchor",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_order_is_stable() {
        assert_eq!(Field::ALL.len(), 4);
        assert_eq!(Field::Title.index(), 0);
        assert_eq!(Field::Anchor.index(), 3);
        assert_eq!(Field::Body.name(), "body");
    }
}
