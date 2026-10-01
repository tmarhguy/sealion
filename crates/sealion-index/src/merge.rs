//! Incremental updates and segment merging (spec §23–24, milestone 06).
//!
//! Generations: segments publish in manifest order (oldest → newest); a
//! newer segment shadows an older one for the same [`DocId`]. Deletes are
//! tombstones in the manifest until a merge drops them permanently.
//!
//! Merge preserves search results: the merged segment contains exactly the
//! live document set (newest version wins, tombstones excluded) with
//! recomputed statistics, so `merge(segments) == clean rebuild`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sealion_core::config::AnalysisConfig;
use sealion_core::document::DocId;
use sealion_core::error::{Error, Result};

use crate::mem_index::MemIndex;
use crate::segment::manifest::Manifest;
use crate::segment::reader::SegmentReader;
use crate::segment::writer::write_segment;

/// Live document set across segments: newest version wins, tombstones out.
/// Returns `(doc_id → newest Document)` in sorted order.
pub fn live_documents(readers: &[SegmentReader], tombstones: &[u64]) -> Vec<DocId> {
    let tomb: BTreeSet<u64> = tombstones.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut live = Vec::new();
    // Walk newest → oldest so first sighting wins.
    for r in readers.iter().rev() {
        for id in r.all_doc_ids() {
            if tomb.contains(&id.0) || !seen.insert(id.0) {
                continue;
            }
            live.push(id);
        }
    }
    live.sort();
    live
}

/// Build a merged [`MemIndex`] from segment readers.
///
/// Semantics: for each DocId keep the newest stored Document, drop
/// tombstoned ids, re-analyze through `analysis`. Statistics are recomputed
/// by `MemIndex` itself, so BM25 inputs stay correct (milestone 07).
pub fn merge_into_memindex(
    readers: &[SegmentReader],
    tombstones: &[u64],
    analysis: &AnalysisConfig,
) -> MemIndex {
    let tomb: BTreeSet<u64> = tombstones.iter().copied().collect();
    // Newest → oldest; insert only first sighting.
    let mut newest: BTreeMap<DocId, _> = BTreeMap::new();
    for r in readers.iter().rev() {
        for id in r.all_doc_ids() {
            if tomb.contains(&id.0) || newest.contains_key(&id) {
                continue;
            }
            if let Some(doc) = r.get(id) {
                newest.insert(id, doc.clone());
            }
        }
    }
    let mut out = MemIndex::new(analysis.clone());
    for (_, doc) in newest {
        out.add_document(doc);
    }
    out
}

/// Merge all segments in `dir` into one new segment, atomically published.
///
/// Steps (crash-safe, queries stay available — readers hold old files):
/// `collect live docs → write+verify new segment → add new +
/// remove olds in one manifest.store → fsync → delete old files`.
/// A crash before `store` leaves the old generation visible; a crash after
/// `store` but before old-file deletion leaves orphans, which are harmless
/// (unreferenced files) and cleaned on the next merge.
pub fn merge_all_segments(dir: &Path, analysis: &AnalysisConfig) -> Result<String> {
    let manifest = Manifest::load_or_new(dir)?;
    if manifest.segments.len() <= 1 && manifest.deleted.is_empty() {
        return Err(Error::Internal(
            "nothing to merge: need 2+ segments or pending tombstones".into(),
        ));
    }
    let mut readers = Vec::new();
    for name in &manifest.segments {
        readers.push(SegmentReader::open(&dir.join(name))?);
    }
    let merged = merge_into_memindex(&readers, &manifest.deleted, analysis);
    if merged.is_empty() {
        return Err(Error::Internal(
            "merge would produce an empty index; refusing to drop all data".into(),
        ));
    }
    // Write + verify + register (reuses crash-safe writer path).
    let meta = write_segment(dir, &merged, true)?;
    // Atomically swap generation: new segment replaces all olds.
    let mut next = Manifest::load_or_new(dir)?;
    // Another writer may have published concurrently; only remove segments
    // that we actually merged (present in our snapshot).
    let merged_set: BTreeSet<&str> = manifest.segments.iter().map(String::as_str).collect();
    next.segments.retain(|s| !merged_set.contains(s.as_str()));
    if !next.segments.contains(&meta.file_name) {
        next.segments.push(meta.file_name.clone());
    }
    next.generation += 1;
    // Tombstones for docs gone from every live segment are now permanent.
    let live_ids: BTreeSet<u64> = merged.all_doc_ids().iter().map(|d| d.0).collect();
    let cleared: Vec<u64> = manifest
        .deleted
        .iter()
        .copied()
        .filter(|d| !live_ids.contains(d))
        .collect();
    // Also clear tombstones that somehow reappeared as live (shouldn't happen).
    next.deleted.retain(|d| live_ids.contains(d));
    let _ = cleared;
    next.store(dir)?;
    // Best-effort orphan cleanup: only files from our snapshot that are no
    // longer referenced. Never delete the new segment.
    let referenced: BTreeSet<&str> = next.segments.iter().map(String::as_str).collect();
    for name in &manifest.segments {
        if !referenced.contains(name.as_str()) {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    Ok(meta.file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::StemmerKind;
    use sealion_core::document::{Document, Source};

    fn cfg() -> AnalysisConfig {
        AnalysisConfig {
            stop_words: false,
            stemmer: StemmerKind::None,
            ..AnalysisConfig::default()
        }
    }

    fn doc(id: u64, body: &str) -> Document {
        Document {
            id: DocId(id),
            source: Source::Synthetic { name: "t".into() },
            url: format!("t://{id}"),
            title: format!("doc {id}"),
            headings: Vec::new(),
            body: body.into(),
            anchor_text: Vec::new(),
            metadata: Default::default(),
            timestamp: 0,
            language: "en".into(),
            content_hash: String::new(),
        }
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sealion-merge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn merge_preserves_live_set_newest_wins() {
        let a = cfg();
        let mut i1 = MemIndex::new(a.clone());
        i1.add_document(doc(1, "alpha"));
        i1.add_document(doc(2, "beta"));
        let mut i2 = MemIndex::new(a.clone());
        i2.add_document(doc(2, "gamma updated"));
        i2.add_document(doc(3, "delta"));

        let dir = tmpdir("newest");
        write_segment(&dir, &i1, true).unwrap();
        // Second write appends seg-000002.
        write_segment(&dir, &i2, true).unwrap();
        let m = Manifest::load_or_new(&dir).unwrap();
        let readers: Vec<_> = m
            .segments
            .iter()
            .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
            .collect();
        let merged = merge_into_memindex(&readers, &[], &a);
        assert_eq!(merged.len(), 3);
        // Doc 2 reflects the newest generation.
        assert!(merged.get(DocId(2)).unwrap().body.contains("gamma"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tombstones_excluded_and_deleted_never_appear() {
        let a = cfg();
        let mut i1 = MemIndex::new(a.clone());
        i1.add_document(doc(1, "alpha beta"));
        i1.add_document(doc(2, "alpha"));
        let merged = {
            let dir = tmpdir("tomb");
            write_segment(&dir, &i1, true).unwrap();
            let m = Manifest::load_or_new(&dir).unwrap();
            let readers: Vec<_> = m
                .segments
                .iter()
                .map(|s| SegmentReader::open(&dir.join(s)).unwrap())
                .collect();
            let out = merge_into_memindex(&readers, &[1], &a);
            let _ = std::fs::remove_dir_all(&dir);
            out
        };
        assert_eq!(merged.len(), 1);
        assert!(merged.get(DocId(1)).is_none());
    }

    #[test]
    fn merge_on_disk_is_atomic_and_preserves_results() {
        let a = cfg();
        let dir = tmpdir("disk");
        let mut i1 = MemIndex::new(a.clone());
        i1.add_document(doc(1, "alpha"));
        let mut i2 = MemIndex::new(a.clone());
        i2.add_document(doc(2, "beta"));
        write_segment(&dir, &i1, true).unwrap();
        write_segment(&dir, &i2, true).unwrap();
        let before = Manifest::load_or_new(&dir).unwrap();
        assert_eq!(before.segments.len(), 2);
        let new_name = merge_all_segments(&dir, &a).unwrap();
        let after = Manifest::load_or_new(&dir).unwrap();
        assert_eq!(after.segments, vec![new_name.clone()]);
        let r = SegmentReader::open(&dir.join(&new_name)).unwrap();
        assert_eq!(r.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
