//! Term dictionary (spec §16): the searchable vocabulary.
//!
//! Design: a **sorted array** of `(field, term)` keys with binary-search
//! lookup and lower-bound prefix enumeration. Justification against the
//! spec's criteria:
//!
//! - *Lookup speed:* `O(log n)` comparisons over a contiguous array; the
//!   resident dictionary of one segment is small enough that pointer-chasing
//!   structures (trie/FST) buy little while complicating the persistent
//!   format.
//! - *Prefix enumeration:* a lower bound seeks the first candidate, then a
//!   linear scan yields completions — exactly what autocomplete (milestone
//!   10) needs, with no second structure.
//! - *Memory:* terms stored once, contiguously; posting metadata rides
//!   alongside each key so dictionary + block directory are one read.
//! - *Build complexity:* sort once at segment write; no balancing, no FST
//!   minimization.
//!
//! A trie/FST representation stays an option if profiling (milestone 16)
//! shows dictionary lookup on the hot path; the on-disk dictionary layout
//! already stores keys sorted to keep that migration format-compatible.

use sealion_core::field::Field;

/// Per-term metadata carried alongside the dictionary key.
///
/// Offsets point into the segment file; `first_doc`/`last_doc` and sizes
/// are the block metadata that later enables skipping and Block-Max WAND
/// (§22). Score upper bounds arrive with BM25 statistics (milestone 07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictEntry {
    pub field: Field,
    pub term: String,
    pub doc_freq: u64,
    pub postings_offset: u64,
    pub postings_len: u64,
    pub positions_offset: u64,
    pub positions_len: u64,
    pub first_doc: u64,
    pub last_doc: u64,
    pub postings_crc: u32,
    pub positions_crc: u32,
}

impl DictEntry {
    pub fn key(&self) -> (Field, &str) {
        (self.field, self.term.as_str())
    }
}

/// Sorted, searchable term dictionary.
#[derive(Debug, Clone, Default)]
pub struct TermDictionary {
    entries: Vec<DictEntry>,
}

impl TermDictionary {
    /// Build from unsorted entries; sorts by `(field, term)`.
    pub fn new(mut entries: Vec<DictEntry>) -> Self {
        entries.sort_by(|a, b| (a.field, &a.term).cmp(&(b.field, &b.term)));
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[DictEntry] {
        &self.entries
    }

    /// Exact lookup by `(field, term)`.
    pub fn lookup(&self, field: Field, term: &str) -> Option<&DictEntry> {
        self.entries
            .binary_search_by(|e| (e.field, e.term.as_str()).cmp(&(field, term)))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// All entries for one field with `term` as prefix, in sort order.
    /// Backs ranked autocomplete (milestone 10); ranking happens there.
    pub fn prefix_entries(&self, field: Field, prefix: &str) -> &[DictEntry] {
        let start = self.entries.partition_point(|e| (e.field, e.term.as_str()) < (field, prefix));
        let mut end = start;
        while end < self.entries.len()
            && self.entries[end].field == field
            && self.entries[end].term.starts_with(prefix)
        {
            end += 1;
        }
        &self.entries[start..end]
    }

    /// All terms for one field, in sort order.
    pub fn field_terms(&self, field: Field) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.field == field)
            .map(|e| e.term.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(field: Field, term: &str, df: u64) -> DictEntry {
        DictEntry {
            field,
            term: term.into(),
            doc_freq: df,
            postings_offset: 0,
            postings_len: 0,
            positions_offset: 0,
            positions_len: 0,
            first_doc: 0,
            last_doc: 0,
            postings_crc: 0,
            positions_crc: 0,
        }
    }

    fn sample() -> TermDictionary {
        TermDictionary::new(vec![
            entry(Field::Body, "database", 3),
            entry(Field::Title, "compiler", 2),
            entry(Field::Body, "compiler", 5),
            entry(Field::Body, "data", 7),
            entry(Field::Body, "datagram", 1),
        ])
    }

    #[test]
    fn lookup_is_field_scoped() {
        let d = sample();
        assert_eq!(d.lookup(Field::Body, "compiler").unwrap().doc_freq, 5);
        assert_eq!(d.lookup(Field::Title, "compiler").unwrap().doc_freq, 2);
        assert!(d.lookup(Field::Body, "missing").is_none());
        // Same term text in another field does not leak across fields.
        assert!(d.lookup(Field::Title, "database").is_none());
    }

    #[test]
    fn entries_sort_by_field_then_term() {
        let d = sample();
        let keys: Vec<(Field, &str)> = d.entries().iter().map(|e| e.key()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn prefix_enumeration_stays_in_field() {
        let d = sample();
        let terms: Vec<&str> = d.prefix_entries(Field::Body, "data").iter().map(|e| e.term.as_str()).collect();
        assert_eq!(terms, vec!["data", "database", "datagram"]);
        assert!(d.prefix_entries(Field::Title, "data").is_empty());
        assert!(d.prefix_entries(Field::Body, "zzz").is_empty());
    }
}
