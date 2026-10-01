//! Small integer primitives for postings storage (spec §20–21).
//!
//! - **Varint:** unsigned LEB128. Compact for the small deltas that dominate
//!   posting data; decode is a tight byte loop.
//! - **Delta codec:** sorted `u64` sequences (DocIDs, within-doc positions)
//!   stored as first-value + successive differences, each varint-encoded.
//!
//! Bit-packed / Frame-of-Reference / SIMD block formats are future work
//! (ADR 005); this module is the baseline they must beat, and the segment
//! format carries a compression-kind flag so codecs stay swappable.

/// Encode one `u64` as unsigned LEB128 into `out`.
pub fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut b = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
            out.push(b);
        } else {
            out.push(b);
            break;
        }
    }
}

/// Decode one unsigned LEB128 from `data`. Returns `(value, bytes_read)`.
/// Returns `None` on truncation or overflow (values must fit in 64 bits).
pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    for (i, &b) in data.iter().enumerate() {
        if i >= 10 {
            return None; // more than 10 bytes cannot fit in a u64
        }
        let bits = (b & 0x7F) as u64;
        if shift == 63 && bits > 1 {
            return None; // only one payload bit fits in the 10th byte
        }
        result |= bits << shift;
        if b & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
    }
    None
}

/// Encode a strictly increasing `u64` sequence as first-value + deltas.
pub fn encode_deltas(sorted: &[u64], out: &mut Vec<u8>) {
    encode_varint(sorted.len() as u64, out);
    let mut prev = 0u64;
    for (i, &v) in sorted.iter().enumerate() {
        debug_assert!(i == 0 || v > prev, "encode_deltas requires strictly increasing input");
        if i == 0 {
            encode_varint(v, out);
        } else {
            encode_varint(v - prev, out);
        }
        prev = v;
    }
}

/// Decode [`encode_deltas`]. Returns `None` on malformed input.
/// Enforces strictly increasing values so corrupt data cannot silently
/// produce a wrong (but plausible) posting list.
pub fn decode_deltas(data: &[u8]) -> Option<(Vec<u64>, usize)> {
    let (n, mut pos) = decode_varint(data)?;
    let n = n as usize;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    let mut prev = 0u64;
    for i in 0..n {
        let (v, used) = decode_varint(data.get(pos..)?)?;
        pos += used;
        let value = if i == 0 { v } else { prev.checked_add(v)? };
        if i > 0 && value <= prev {
            return None; // deltas must be positive (strictly increasing)
        }
        out.push(value);
        prev = value;
    }
    Some((out, pos))
}

/// Encoded byte length of one varint value (for size accounting).
pub fn varint_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_known_vectors() {
        let cases: &[(u64, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7F]),
            (128, &[0x80, 0x01]),
            (300, &[0xAC, 0x02]),
            (u64::MAX, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        ];
        for (v, bytes) in cases {
            let mut out = Vec::new();
            encode_varint(*v, &mut out);
            assert_eq!(&out, bytes, "encode {v}");
            assert_eq!(decode_varint(bytes), Some((*v, bytes.len())), "decode {v}");
        }
    }

    #[test]
    fn varint_rejects_truncation_and_overflow() {
        assert_eq!(decode_varint(&[]), None);
        assert_eq!(decode_varint(&[0x80]), None);
        // 11 continuation bytes overflow u64.
        assert_eq!(decode_varint(&[0xFF; 11]), None);
    }

    #[test]
    fn deltas_round_trip() {
        let seq = vec![3u64, 7, 8, 100, 10_000, 1 << 40];
        let mut buf = Vec::new();
        encode_deltas(&seq, &mut buf);
        let (back, used) = decode_deltas(&buf).unwrap();
        assert_eq!(back, seq);
        assert_eq!(used, buf.len());
        // Deltas must beat raw u64s here: 6 values in far fewer than 48 bytes.
        assert!(buf.len() < 6 * 8, "compressed larger than raw: {}", buf.len());
    }

    #[test]
    fn deltas_empty_round_trip() {
        let mut buf = Vec::new();
        encode_deltas(&[], &mut buf);
        assert_eq!(decode_deltas(&buf).unwrap().0, Vec::<u64>::new());
    }

    #[test]
    fn deltas_reject_non_increasing_data() {
        // count=2, values 5 then delta 0 → not strictly increasing.
        assert_eq!(decode_deltas(&[0x02, 0x05, 0x00]), None);
    }

    #[test]
    fn deltas_reject_truncation() {
        let mut buf = Vec::new();
        encode_deltas(&[1, 2, 3], &mut buf);
        buf.pop();
        assert_eq!(decode_deltas(&buf), None);
    }
}
