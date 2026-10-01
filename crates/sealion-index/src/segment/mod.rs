//! Immutable persistent segments (spec §17–19).
//!
//! A segment is one self-contained index file written once and never
//! modified. Readers open published generations concurrently while writers
//! publish new ones; crash safety comes from write-to-temp, verify, and
//! atomic rename of both segment files and the manifest.
//!
//! On-disk layout (all integers little-endian; see `docs/index-format.md`):
//!
//! ```text
//! header (56 B): magic, version, flags, counts, region offsets, header CRC
//! postings + positions data blocks (per-term, offset from dictionary)
//! dictionary: count + sorted entries (field, term, offsets, sizes, CRCs)
//! document metadata: stored Documents as JSON
//! statistics: doc count + per-field token totals (BM25 inputs)
//! footer (16 B): magic, file CRC32, reserved
//! ```

pub mod manifest;

use sealion_core::error::{Error, Result};

/// File magic, also repeated in the footer.
pub const MAGIC: &[u8; 8] = b"SEALION1";
/// Segment format version. Readers reject anything else.
pub const FORMAT_VERSION: u32 = 1;
/// Fixed header length in bytes.
pub const HEADER_LEN: usize = 56;
/// Fixed footer length in bytes.
pub const FOOTER_LEN: usize = 16;

/// Flags bit: postings DocIDs are delta+varint encoded (else raw u64 LE).
pub const FLAG_COMPRESSED_POSTINGS: u32 = 1 << 0;
/// Flags bit: positions are delta+varint encoded (else raw u64 LE).
pub const FLAG_COMPRESSED_POSITIONS: u32 = 1 << 1;
/// Flags used by production writes (everything compressed).
pub const FLAGS_DEFAULT: u32 = FLAG_COMPRESSED_POSTINGS | FLAG_COMPRESSED_POSITIONS;

/// Segment file extension.
pub const SEGMENT_EXT: &str = "seal";
/// Manifest file name inside the data directory.
pub const MANIFEST_NAME: &str = "manifest.json";

pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn get_u32(data: &[u8], pos: &mut usize) -> Result<u32> {
    let end = pos.checked_add(4).ok_or_else(|| Error::Corrupt("offset overflow".into()))?;
    if end > data.len() {
        return Err(Error::Corrupt("truncated u32".into()));
    }
    let v = u32::from_le_bytes(data[*pos..end].try_into().expect("4 bytes"));
    *pos = end;
    Ok(v)
}

pub fn get_u64(data: &[u8], pos: &mut usize) -> Result<u64> {
    let end = pos.checked_add(8).ok_or_else(|| Error::Corrupt("offset overflow".into()))?;
    if end > data.len() {
        return Err(Error::Corrupt("truncated u64".into()));
    }
    let v = u64::from_le_bytes(data[*pos..end].try_into().expect("8 bytes"));
    *pos = end;
    Ok(v)
}

pub fn get_bytes<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = pos.checked_add(len).ok_or_else(|| Error::Corrupt("offset overflow".into()))?;
    if end > data.len() {
        return Err(Error::Corrupt("truncated bytes".into()));
    }
    let s = &data[*pos..end];
    *pos = end;
    Ok(s)
}
