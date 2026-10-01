//! SeaLion inverted index: segments, postings, dictionary, compression,
//! merging, and storage (spec §14–25).
//!
//! Milestone 02 delivers the text analysis pipeline (`analysis`, `stemmer`).
//! Milestone 03 adds the in-memory index.

pub mod analysis;
pub mod checksum;
pub mod codec;
pub mod dictionary;
pub mod mem_index;
pub mod segment;
pub mod stemmer;
