//! Segment reader: verified, concurrent-friendly segment access.
//!
//! `open` performs full integrity validation (header, footer, file CRC,
//! every block CRC, structural bounds), so any successfully opened reader
//! is safe to query. Files are read fully into memory for now; the access
//! pattern (offset slices, no mutation) is ready for mmap when segments
//! outgrow RAM.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sealion_core::document::{DocId, Document};
use sealion_core::error::{Error, Result};
use sealion_core::field::Field;

use super::{
    get_bytes, get_u32, get_u64, FLAG_COMPRESSED_POSITIONS, FLAG_COMPRESSED_POSTINGS, FOOTER_LEN,
    FORMAT_VERSION, HEADER_LEN, MAGIC,
};
use crate::checksum::crc32;
use crate::codec::decode_deltas;
use crate::dictionary::{DictEntry, TermDictionary};
use crate::mem_index::{MemIndex, Posting};

/// Collection statistics stored in the segment (BM25 inputs, milestone 07).
#[derive(Debug, Clone)]
pub struct SegmentStats {
    pub doc_count: u64,
    /// Total emitted tokens per field in canonical field order.
    pub field_tokens: [u64; 4],
}

impl SegmentStats {
    pub fn avg_field_length(&self, field: Field) -> f64 {
        if self.doc_count == 0 {
            return 0.0;
        }
        self.field_tokens[field.index()] as f64 / self.doc_count as f64
    }
}

/// An opened, fully verified segment.
#[derive(Debug, Clone)]
pub struct SegmentReader {
    path: PathBuf,
    flags: u32,
    dict: TermDictionary,
    docs: BTreeMap<DocId, Document>,
    doc_ids: Vec<DocId>,
    stats: SegmentStats,
    file_bytes: usize,
    // Raw file retained for block slicing (borrowed by postings calls).
    data: Vec<u8>,
}

impl SegmentReader {
    /// Open and fully verify a segment file.
    pub fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).map_err(|e| Error::Io(e.to_string()))?;
        Self::from_bytes(data, path.to_path_buf())
    }

    fn from_bytes(data: Vec<u8>, path: PathBuf) -> Result<Self> {
        if data.len() < HEADER_LEN + FOOTER_LEN {
            return Err(Error::Corrupt("file smaller than header + footer".into()));
        }
        // Header.
        let mut pos = 0;
        let magic = get_bytes(&data, &mut pos, 8)?;
        if magic != MAGIC {
            return Err(Error::Corrupt("bad segment magic".into()));
        }
        let version = get_u32(&data, &mut pos)?;
        if version != FORMAT_VERSION {
            return Err(Error::Corrupt(format!(
                "unsupported segment version {version}"
            )));
        }
        let flags = get_u32(&data, &mut pos)?;
        if flags & !(FLAG_COMPRESSED_POSTINGS | FLAG_COMPRESSED_POSITIONS) != 0 {
            return Err(Error::Corrupt(format!("unknown segment flags {flags:#x}")));
        }
        let doc_count = get_u64(&data, &mut pos)?;
        let dict_count = get_u64(&data, &mut pos)?;
        let dict_offset = get_u64(&data, &mut pos)?;
        let meta_offset = get_u64(&data, &mut pos)?;
        let stats_offset = get_u64(&data, &mut pos)?;
        let header_crc = get_u32(&data, &mut pos)?;
        debug_assert_eq!(pos, HEADER_LEN);
        if crc32(&data[..HEADER_LEN - 4]) != header_crc {
            return Err(Error::Corrupt("header CRC mismatch".into()));
        }

        // Footer.
        let footer_pos = data.len() - FOOTER_LEN;
        if &data[footer_pos..footer_pos + 8] != MAGIC {
            return Err(Error::Corrupt("bad segment footer magic".into()));
        }
        let file_crc = u32::from_le_bytes(
            data[footer_pos + 8..footer_pos + 12]
                .try_into()
                .expect("4 bytes"),
        );
        if crc32(&data[..footer_pos]) != file_crc {
            return Err(Error::Corrupt("file CRC mismatch".into()));
        }

        // Region sanity: header < dict <= meta <= stats <= footer.
        if !(dict_offset as usize <= meta_offset as usize
            && meta_offset as usize <= stats_offset as usize
            && stats_offset as usize <= footer_pos
            && dict_offset as usize >= HEADER_LEN)
        {
            return Err(Error::Corrupt("segment region offsets out of order".into()));
        }

        // Dictionary.
        let mut dpos = dict_offset as usize;
        let n = get_u64(&data, &mut dpos)?;
        if n != dict_count {
            return Err(Error::Corrupt("dictionary count mismatch".into()));
        }
        let mut entries = Vec::with_capacity(n.min(1 << 20) as usize);
        for _ in 0..n {
            let field = byte_to_field(*get_bytes(&data, &mut dpos, 1)?.first().expect("1 byte"))?;
            let term_len = get_u32(&data, &mut dpos)? as usize;
            let term = get_bytes(&data, &mut dpos, term_len)?;
            let term = std::str::from_utf8(term)
                .map_err(|_| Error::Corrupt("dictionary term is not UTF-8".into()))?
                .to_string();
            entries.push(DictEntry {
                field,
                term,
                doc_freq: get_u64(&data, &mut dpos)?,
                postings_offset: get_u64(&data, &mut dpos)?,
                postings_len: get_u64(&data, &mut dpos)?,
                positions_offset: get_u64(&data, &mut dpos)?,
                positions_len: get_u64(&data, &mut dpos)?,
                first_doc: get_u64(&data, &mut dpos)?,
                last_doc: get_u64(&data, &mut dpos)?,
                postings_crc: get_u32(&data, &mut dpos)?,
                positions_crc: get_u32(&data, &mut dpos)?,
            });
        }
        // Dictionary must be sorted (writer invariant; readers rely on it).
        let dict = TermDictionary::new(entries);
        {
            let keys: Vec<(Field, &str)> = dict.entries().iter().map(|e| e.key()).collect();
            let mut sorted = keys.clone();
            sorted.sort();
            if keys != sorted {
                return Err(Error::Corrupt("dictionary not sorted".into()));
            }
        }

        let this = Self {
            path,
            flags,
            dict,
            docs: BTreeMap::new(),
            doc_ids: Vec::new(),
            stats: SegmentStats {
                doc_count: 0,
                field_tokens: [0; 4],
            },
            file_bytes: data.len(),
            data,
        };
        // Verify blocks + load metadata/stats (needs &self block access).
        let mut this = this;
        this.verify_blocks()?;
        this.load_metadata(meta_offset as usize, doc_count)?;
        this.load_stats(stats_offset as usize, footer_pos)?;
        Ok(this)
    }

    fn block(&self, offset: u64, len: u64, what: &str) -> Result<&[u8]> {
        let start = offset as usize;
        let len = len as usize;
        let end = start
            .checked_add(len)
            .ok_or_else(|| Error::Corrupt(format!("{what} offset overflow")))?;
        if end > self.data.len() {
            return Err(Error::Corrupt(format!("{what} block out of bounds")));
        }
        Ok(&self.data[start..end])
    }

    fn verify_blocks(&self) -> Result<()> {
        for e in self.dict.entries() {
            let p = self.block(e.postings_offset, e.postings_len, "postings")?;
            if crc32(p) != e.postings_crc {
                return Err(Error::Corrupt(format!(
                    "postings CRC mismatch for {:?}",
                    e.key()
                )));
            }
            let q = self.block(e.positions_offset, e.positions_len, "positions")?;
            if crc32(q) != e.positions_crc {
                return Err(Error::Corrupt(format!(
                    "positions CRC mismatch for {:?}",
                    e.key()
                )));
            }
        }
        Ok(())
    }

    fn load_metadata(&mut self, offset: usize, doc_count: u64) -> Result<()> {
        let mut pos = offset;
        let n = get_u64(&self.data, &mut pos)?;
        if n != doc_count {
            return Err(Error::Corrupt("metadata doc count mismatch".into()));
        }
        for _ in 0..n {
            let id = DocId(get_u64(&self.data, &mut pos)?);
            let json_len = get_u32(&self.data, &mut pos)? as usize;
            let json = get_bytes(&self.data, &mut pos, json_len)?;
            let doc: Document = serde_json::from_slice(json)
                .map_err(|e| Error::Corrupt(format!("stored document {id:?}: {e}")))?;
            if doc.id != id {
                return Err(Error::Corrupt("stored document id mismatch".into()));
            }
            self.doc_ids.push(id);
            self.docs.insert(id, doc);
        }
        self.doc_ids.sort();
        Ok(())
    }

    fn load_stats(&mut self, offset: usize, footer_pos: usize) -> Result<()> {
        let mut pos = offset;
        let doc_count = get_u64(&self.data, &mut pos)?;
        let mut field_tokens = [0u64; 4];
        for slot in &mut field_tokens {
            *slot = get_u64(&self.data, &mut pos)?;
        }
        if pos > footer_pos {
            return Err(Error::Corrupt("statistics run past footer".into()));
        }
        if doc_count != self.doc_ids.len() as u64 {
            return Err(Error::Corrupt("statistics doc count mismatch".into()));
        }
        self.stats = SegmentStats {
            doc_count,
            field_tokens,
        };
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dictionary(&self) -> &TermDictionary {
        &self.dict
    }

    pub fn stats(&self) -> &SegmentStats {
        &self.stats
    }

    pub fn file_bytes(&self) -> usize {
        self.file_bytes
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn all_doc_ids(&self) -> Vec<DocId> {
        self.doc_ids.clone()
    }

    pub fn get(&self, id: DocId) -> Option<&Document> {
        self.docs.get(&id)
    }

    /// Decode one term's posting list with positions.
    pub fn postings(&self, field: Field, term: &str) -> Result<Vec<Posting>> {
        let e = match self.dict.lookup(field, term) {
            Some(e) => e,
            None => return Ok(Vec::new()),
        };
        let p_block = self.block(e.postings_offset, e.postings_len, "postings")?;
        if crc32(p_block) != e.postings_crc {
            return Err(Error::Corrupt("postings CRC mismatch on read".into()));
        }
        let doc_ids = if self.flags & FLAG_COMPRESSED_POSTINGS != 0 {
            decode_deltas(p_block)
                .ok_or_else(|| Error::Corrupt("malformed postings block".into()))?
                .0
        } else {
            decode_raw_u64s(p_block, "postings")?
        };

        let q_block = self.block(e.positions_offset, e.positions_len, "positions")?;
        if crc32(q_block) != e.positions_crc {
            return Err(Error::Corrupt("positions CRC mismatch on read".into()));
        }
        let positions = if self.flags & FLAG_COMPRESSED_POSITIONS != 0 {
            decode_position_lists(q_block, doc_ids.len())?
        } else {
            decode_raw_position_lists(q_block, doc_ids.len())?
        };

        if doc_ids.len() != positions.len() {
            return Err(Error::Corrupt("postings/positions length mismatch".into()));
        }
        let mut out = Vec::with_capacity(doc_ids.len());
        for (id, plist) in doc_ids.into_iter().zip(positions) {
            let mut positions = Vec::with_capacity(plist.len());
            for p in plist {
                positions.push(
                    u32::try_from(p)
                        .map_err(|_| Error::Corrupt("position overflows u32".into()))?,
                );
            }
            out.push(Posting {
                doc: DocId(id),
                positions,
            });
        }
        Ok(out)
    }

    /// Verify this segment against its source index: counts plus every
    /// posting list. Used by the writer before atomic publication.
    pub fn verify_full(&self, index: &MemIndex) -> Result<()> {
        if self.len() != index.len() {
            return Err(Error::Corrupt("verify: doc count mismatch".into()));
        }
        if self.dict.len() != index.term_count() {
            return Err(Error::Corrupt("verify: term count mismatch".into()));
        }
        for (field, term, expected) in index.iter_postings() {
            let actual = self.postings(field, term)?;
            if actual != expected {
                return Err(Error::Corrupt(format!(
                    "verify: postings mismatch for {field:?}:{term}"
                )));
            }
        }
        Ok(())
    }
}

fn byte_to_field(b: u8) -> Result<Field> {
    match b {
        0 => Ok(Field::Title),
        1 => Ok(Field::Heading),
        2 => Ok(Field::Body),
        3 => Ok(Field::Anchor),
        _ => Err(Error::Corrupt(format!("unknown field id {b}"))),
    }
}

fn decode_raw_u64s(block: &[u8], what: &str) -> Result<Vec<u64>> {
    if block.len() < 8 {
        return Err(Error::Corrupt(format!("truncated raw {what} block")));
    }
    let n = u64::from_le_bytes(block[0..8].try_into().expect("8 bytes")) as usize;
    let mut pos = 8;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        if pos + 8 > block.len() {
            return Err(Error::Corrupt(format!("truncated raw {what} block")));
        }
        out.push(u64::from_le_bytes(
            block[pos..pos + 8].try_into().expect("8 bytes"),
        ));
        pos += 8;
    }
    if pos != block.len() {
        return Err(Error::Corrupt(format!(
            "trailing bytes in raw {what} block"
        )));
    }
    Ok(out)
}

fn decode_position_lists(block: &[u8], n: usize) -> Result<Vec<Vec<u64>>> {
    let mut out = Vec::with_capacity(n);
    let mut pos = 0;
    for _ in 0..n {
        let (list, used) = decode_deltas(block.get(pos..).unwrap_or(&[]))
            .ok_or_else(|| Error::Corrupt("malformed positions block".into()))?;
        pos += used;
        out.push(list);
    }
    if pos != block.len() {
        return Err(Error::Corrupt("trailing bytes in positions block".into()));
    }
    Ok(out)
}

fn decode_raw_position_lists(block: &[u8], n: usize) -> Result<Vec<Vec<u64>>> {
    let mut out = Vec::with_capacity(n);
    let mut pos = 0;
    for _ in 0..n {
        if pos + 8 > block.len() {
            return Err(Error::Corrupt("truncated raw positions block".into()));
        }
        let m = u64::from_le_bytes(block[pos..pos + 8].try_into().expect("8 bytes")) as usize;
        pos += 8;
        let mut list = Vec::with_capacity(m.min(1 << 20));
        for _ in 0..m {
            if pos + 8 > block.len() {
                return Err(Error::Corrupt("truncated raw positions block".into()));
            }
            list.push(u64::from_le_bytes(
                block[pos..pos + 8].try_into().expect("8 bytes"),
            ));
            pos += 8;
        }
        out.push(list);
    }
    if pos != block.len() {
        return Err(Error::Corrupt(
            "trailing bytes in raw positions block".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_field_id_is_corrupt() {
        assert!(byte_to_field(9).is_err());
    }

    #[test]
    fn raw_decoders_reject_truncation_and_trailing_bytes() {
        // count=1, one full u64 value → ok.
        let ok = [1u8, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0];
        assert!(decode_raw_u64s(&ok, "t").is_ok());
        // Missing value bytes → truncated.
        assert!(decode_raw_u64s(&ok[..9], "t").is_err());
        // Extra byte → trailing.
        let mut trailing = ok.to_vec();
        trailing.push(0);
        assert!(decode_raw_u64s(&trailing, "t").is_err());
    }
}
