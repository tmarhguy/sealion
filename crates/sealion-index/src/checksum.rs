//! Integrity primitives for persistent segments (spec §18–19, §99).
//!
//! - **CRC32 (ISO 3309):** block and file checksums. Table-driven,
//!   hand-implemented so segments have zero new dependencies. Verified
//!   against the standard check vector (`"123456789"` → `0xCBF43926`).
//! - **FNV-1a 64:** non-cryptographic hash for stable DocIds and content
//!   hashes in local-corpus ingestion. Not a checksum substitute.

const CRC_TABLE: [u32; 256] = make_crc_table();

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 == 1 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// CRC32 (ISO 3309) over `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// 64-bit FNV-1a hash. Deterministic across runs; used for stable DocIds
/// (`hash(canonical path)`) and content hashes (`hash(content)`).
pub fn fnv1a64(data: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let mut h = OFFSET;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Lowercase hex of [`fnv1a64`], for `content_hash`-style string fields.
pub fn fnv1a64_hex(data: &[u8]) -> String {
    format!("{:016x}", fnv1a64(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_standard_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn crc32_empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn crc32_detects_single_bit_flip() {
        let data = b"posting block payload";
        let mut corrupt = data.to_vec();
        corrupt[7] ^= 0x01;
        assert_ne!(crc32(data), crc32(&corrupt));
    }

    #[test]
    fn fnv_is_deterministic_and_sensitive() {
        assert_eq!(fnv1a64(b"corpus/a.txt"), fnv1a64(b"corpus/a.txt"));
        assert_ne!(fnv1a64(b"corpus/a.txt"), fnv1a64(b"corpus/b.txt"));
        assert_eq!(fnv1a64_hex(b"abc").len(), 16);
    }
}
