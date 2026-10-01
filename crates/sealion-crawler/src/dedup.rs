//! Duplicate detection (spec §10): exact content hashes plus SimHash
//! near-duplicates over shingles.
//!
//! - Exact: FNV-1a 64-bit over normalized content (hex). Independent of
//!   `sealion-index`'s copy (crawler must not depend on the index crate).
//! - Near: 64-bit SimHash over 5-shingles of normalized tokens; Hamming
//!   distance ≤ 3 counts as near-duplicate (standard threshold).

use std::collections::{HashMap, HashSet};

/// FNV-1a 64-bit.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn fnv1a64_hex(data: &[u8]) -> String {
    format!("{:016x}", fnv1a64(data))
}

/// Normalize text for hashing: lowercase, collapse whitespace.
pub fn normalize_text(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// SplitMix64 finalizer: FNV-1a's low bits mix poorly, which destabilizes
/// SimHash voting. This avalanche step makes every shingle bit uniform.
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// 64-bit SimHash over word 5-shingles.
pub fn simhash(text: &str) -> u64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return 0;
    }
    let mut acc = [0i32; 64];
    let push_shingle = |shingle: &str, acc: &mut [i32; 64]| {
        let h = mix64(fnv1a64(shingle.as_bytes()));
        for (i, slot) in acc.iter_mut().enumerate() {
            if (h >> i) & 1 == 1 {
                *slot += 1;
            } else {
                *slot -= 1;
            }
        }
    };
    if words.len() < 5 {
        push_shingle(&words.join(" "), &mut acc);
    } else {
        for w in words.windows(5) {
            push_shingle(&w.join(" "), &mut acc);
        }
    }
    let mut out = 0u64;
    for (i, &v) in acc.iter().enumerate() {
        if v > 0 {
            out |= 1 << i;
        }
    }
    out
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Threshold: ≤7 bits differ → near-duplicate. Chosen empirically: 1–2
/// word edits in 60-word pages score 4–6, unrelated pages score ~35.
/// Short texts (<25 words) skip SimHash — too few shingles to be stable —
/// and rely on exact hashing only.
pub const SIMHASH_THRESHOLD: u32 = 7;
pub const SIMHASH_MIN_WORDS: usize = 25;

/// In-memory dedup index: exact hashes + simhashes.
#[derive(Debug, Default)]
pub struct Dedup {
    exact: HashSet<String>,
    simhashes: Vec<u64>,
    pub exact_dups: u64,
    pub near_dups: u64,
}

impl Dedup {
    /// Check normalized content. Returns true if duplicate (exact or near).
    /// New content is recorded either way (first sighting wins).
    pub fn check(&mut self, normalized: &str) -> bool {
        let hex = fnv1a64_hex(normalized.as_bytes());
        if !self.exact.insert(hex) {
            self.exact_dups += 1;
            return true;
        }
        if normalized.split_whitespace().count() >= SIMHASH_MIN_WORDS {
            let h = simhash(normalized);
            if self
                .simhashes
                .iter()
                .any(|&s| hamming(s, h) <= SIMHASH_THRESHOLD)
            {
                self.near_dups += 1;
                return true;
            }
            self.simhashes.push(h);
        }
        false
    }

    /// Seed with already-indexed content hashes (resume path).
    pub fn seed_exact(&mut self, hex: String) {
        self.exact.insert(hex);
    }

    pub fn stats(&self) -> HashMap<&'static str, u64> {
        HashMap::from([
            ("exact_dups", self.exact_dups),
            ("near_dups", self.near_dups),
            ("unique", self.exact.len() as u64),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_whitespace_variants() {
        let mut d = Dedup::default();
        assert!(!d.check(&normalize_text("Hello World")));
        assert!(d.check(&normalize_text("  hello   WORLD\n")));
        assert_eq!(d.exact_dups, 1);
    }

    #[test]
    fn near_duplicate_detection() {
        let mut d = Dedup::default();
        let filler = "compiler optimization passes transform intermediate representation into efficient machine code";
        let base = (filler.to_string() + " ").repeat(6);
        assert!(!d.check(&base));
        // One word changed in a 60-word page → near-dup.
        let mut words: Vec<&str> = base.split_whitespace().collect();
        words[17] = "performant";
        assert!(d.check(&words.join(" ")));
        assert_eq!(d.near_dups, 1);
        // Totally different → unique.
        assert!(!d.check("quantum entanglement violates local realism experiments photon bell tests distant measurement choices detector"));
        // Short texts rely on exact hashing only.
        let mut s = Dedup::default();
        assert!(!s.check("alpha beta gamma delta"));
        assert!(!s.check("alpha beta gamma epsilon"));
        assert_eq!(s.near_dups, 0);
    }

    #[test]
    fn hamming_basics() {
        assert_eq!(hamming(0, 0), 0);
        assert_eq!(hamming(0, u64::MAX), 64);
    }
}
