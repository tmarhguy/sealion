//! Segment writer: MemIndex → immutable segment file (spec §17–19).
//!
//! Crash-safe publication: the segment is assembled fully in memory,
//! written to a temp file in the target directory, `fsync`ed, verified by
//! reading it back through the reader (checksums, counts, spot postings),
//! then atomically renamed into place and registered in the manifest.
//! A crash at any point leaves only temp files, never a partially visible
//! segment.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sealion_core::error::{Error, Result};
use sealion_core::field::Field;

use super::manifest::Manifest;
use super::reader::SegmentReader;
use super::{
    put_u32, put_u64, FLAGS_DEFAULT, FLAG_COMPRESSED_POSITIONS, FLAG_COMPRESSED_POSTINGS,
    FORMAT_VERSION, HEADER_LEN, MAGIC, SEGMENT_EXT,
};
use crate::checksum::crc32;
use crate::codec::encode_deltas;
use crate::dictionary::DictEntry;
use crate::mem_index::{MemIndex, Posting};

/// Metadata about a published segment, as recorded in the manifest.
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub file_name: String,
    pub doc_count: usize,
    pub term_count: usize,
    pub bytes: u64,
}

/// Write `index` as one new segment in `dir`, publish it crash-safely,
/// and register it in the manifest. Returns the published metadata.
///
/// `compress` selects delta+varint postings (`true`, production) versus raw
/// u64 storage (`false`, compression-benchmark baseline only).
pub fn write_segment(dir: &Path, index: &MemIndex, compress: bool) -> Result<SegmentMeta> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
    let flags = if compress { FLAGS_DEFAULT } else { 0 };

    let file_bytes = build_file(index, flags)?;
    let tmp = temp_path(dir)?;
    let bytes = file_bytes.len() as u64;
    {
        let mut f = File::create(&tmp).map_err(|e| Error::Io(e.to_string()))?;
        f.write_all(&file_bytes)
            .map_err(|e| Error::Io(e.to_string()))?;
        f.sync_all().map_err(|e| Error::Io(e.to_string()))?;
    }
    // Verify before publish: full reader-side validation of the temp file.
    let probe = SegmentReader::open(&tmp)?;
    probe.verify_full(index)?;

    let manifest = Manifest::load_or_new(dir)?;
    let file_name = format!("seg-{:06}.{}", manifest.next_id(), SEGMENT_EXT);
    let dest = dir.join(&file_name);
    std::fs::rename(&tmp, &dest).map_err(|e| Error::Io(e.to_string()))?;
    // fsync the directory so the rename itself survives a crash.
    sync_dir(dir)?;

    let mut manifest = manifest;
    manifest.add_segment(file_name.clone());
    manifest.store(dir)?;

    Ok(SegmentMeta {
        file_name,
        doc_count: index.len(),
        term_count: index.term_count(),
        bytes,
    })
}

/// Assemble the complete segment file image in memory.
fn build_file(index: &MemIndex, flags: u32) -> Result<Vec<u8>> {
    let compressed_postings = flags & FLAG_COMPRESSED_POSTINGS != 0;
    let compressed_positions = flags & FLAG_COMPRESSED_POSITIONS != 0;

    // Data blocks first; dictionary records their file offsets.
    let mut data = Vec::new();
    let mut entries: Vec<DictEntry> = Vec::new();
    for (field, term, postings) in index.iter_postings() {
        let doc_ids: Vec<u64> = postings.iter().map(|p| p.doc.0).collect();
        let postings_offset = HEADER_LEN as u64 + data.len() as u64;
        let postings_block = encode_postings_block(&doc_ids, compressed_postings);
        let postings_crc = crc32(&postings_block);
        data.extend_from_slice(&postings_block);

        let positions_offset = HEADER_LEN as u64 + data.len() as u64;
        let positions_block = encode_positions_block(postings, compressed_positions);
        let positions_crc = crc32(&positions_block);
        data.extend_from_slice(&positions_block);

        entries.push(DictEntry {
            field,
            term: term.to_string(),
            doc_freq: postings.len() as u64,
            postings_offset,
            postings_len: postings_block.len() as u64,
            positions_offset,
            positions_len: positions_block.len() as u64,
            first_doc: doc_ids.first().copied().unwrap_or(0),
            last_doc: doc_ids.last().copied().unwrap_or(0),
            postings_crc,
            positions_crc,
        });
    }

    // Dictionary region.
    let dict_offset = HEADER_LEN as u64 + data.len() as u64;
    let mut dict = Vec::new();
    put_u64(&mut dict, entries.len() as u64);
    for e in &entries {
        dict.push(field_to_byte(e.field)?);
        put_u32(&mut dict, e.term.len() as u32);
        dict.extend_from_slice(e.term.as_bytes());
        put_u64(&mut dict, e.doc_freq);
        put_u64(&mut dict, e.postings_offset);
        put_u64(&mut dict, e.postings_len);
        put_u64(&mut dict, e.positions_offset);
        put_u64(&mut dict, e.positions_len);
        put_u64(&mut dict, e.first_doc);
        put_u64(&mut dict, e.last_doc);
        put_u32(&mut dict, e.postings_crc);
        put_u32(&mut dict, e.positions_crc);
    }

    // Document metadata region: stored Documents as JSON.
    let meta_offset = dict_offset + dict.len() as u64;
    let mut meta = Vec::new();
    let docs: Vec<_> = index.iter_docs().collect();
    put_u64(&mut meta, docs.len() as u64);
    for d in &docs {
        let json = serde_json::to_vec(d).map_err(|e| Error::Internal(e.to_string()))?;
        put_u64(&mut meta, d.id.0);
        put_u32(&mut meta, json.len() as u32);
        meta.extend_from_slice(&json);
    }

    // Statistics region: doc count + per-field token totals (BM25 inputs).
    let stats_offset = meta_offset + meta.len() as u64;
    let mut stats = Vec::new();
    put_u64(&mut stats, docs.len() as u64);
    for f in Field::ALL {
        put_u64(&mut stats, index.field_token_total(f));
    }

    // Header with offsets + CRC over the preceding 52 bytes.
    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    put_u32(&mut header, FORMAT_VERSION);
    put_u32(&mut header, flags);
    put_u64(&mut header, docs.len() as u64);
    put_u64(&mut header, entries.len() as u64);
    put_u64(&mut header, dict_offset);
    put_u64(&mut header, meta_offset);
    put_u64(&mut header, stats_offset);
    debug_assert_eq!(header.len(), HEADER_LEN - 4);
    let header_crc = crc32(&header);
    put_u32(&mut header, header_crc);

    let mut file = header;
    file.extend_from_slice(&data);
    file.extend_from_slice(&dict);
    file.extend_from_slice(&meta);
    file.extend_from_slice(&stats);

    // Footer: magic + CRC32 of everything before it + reserved.
    let file_crc = crc32(&file);
    file.extend_from_slice(MAGIC);
    put_u32(&mut file, file_crc);
    put_u32(&mut file, 0);

    Ok(file)
}

fn encode_postings_block(doc_ids: &[u64], compressed: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if compressed {
        encode_deltas(doc_ids, &mut out);
    } else {
        put_u64(&mut out, doc_ids.len() as u64);
        for &id in doc_ids {
            put_u64(&mut out, id);
        }
    }
    out
}

fn encode_positions_block(postings: &[Posting], compressed: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if compressed {
        for p in postings {
            let ps: Vec<u64> = p.positions.iter().map(|&x| u64::from(x)).collect();
            encode_deltas(&ps, &mut out);
        }
    } else {
        for p in postings {
            put_u64(&mut out, p.positions.len() as u64);
            for &pos in &p.positions {
                put_u64(&mut out, u64::from(pos));
            }
        }
    }
    out
}

fn field_to_byte(field: Field) -> Result<u8> {
    Ok(match field {
        Field::Title => 0,
        Field::Heading => 1,
        Field::Body => 2,
        Field::Anchor => 3,
    })
}

fn temp_path(dir: &Path) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Internal(e.to_string()))?
        .as_nanos();
    Ok(dir.join(format!("seg-{}-{}.tmp", std::process::id(), nanos)))
}

fn sync_dir(dir: &Path) -> Result<()> {
    let f = File::open(dir).map_err(|e| Error::Io(e.to_string()))?;
    f.sync_all().map_err(|e| Error::Io(e.to_string()))?;
    Ok(())
}
