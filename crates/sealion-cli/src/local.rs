//! Local corpus ingestion and persistent-index commands (spec §6).
//!
//! Supported today: `.txt` (title = file stem) and `.md` (title = first
//! `#` heading, else stem; heading markers stripped naively). HTML,
//! JSON records, and source-code ingestion arrive with later milestones;
//! other files are counted and skipped honestly.
//!
//! Document IDs are stable: `fnv1a64(canonical path)`, so re-indexing the
//! same tree replaces the same IDs instead of duplicating them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};

use sealion_index::analysis::Analyzer;
use sealion_index::checksum::{fnv1a64, fnv1a64_hex};
use sealion_index::mem_index::MemIndex;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_query::execute::{search_view, union_sorted};
use sealion_query::query::Query;

/// A harvested corpus file.
struct RawFile {
    path: PathBuf,
    title: String,
    body: String,
}

/// Recursively collect `.txt`/`.md` files, counting skipped formats.
fn collect_files(root: &Path) -> Result<(Vec<RawFile>, usize)> {
    let mut files = Vec::new();
    let mut skipped = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("cannot list {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read entry in {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                match path.extension().and_then(|e| e.to_str()) {
                    Some("txt") => {
                        let body = std::fs::read_to_string(&path)
                            .with_context(|| format!("cannot read {}", path.display()))?;
                        let title = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("untitled")
                            .to_string();
                        files.push(RawFile { path, title, body });
                    }
                    Some("md") => {
                        let text = std::fs::read_to_string(&path)
                            .with_context(|| format!("cannot read {}", path.display()))?;
                        let (title, body) = parse_markdown(&path, &text);
                        files.push(RawFile { path, title, body });
                    }
                    _ => skipped += 1,
                }
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((files, skipped))
}

/// Naive Markdown handling: first `#` heading wins as title, markers and
/// fences stripped. Real Markdown/HTML extraction is milestone 11 work.
fn parse_markdown(path: &Path, text: &str) -> (String, String) {
    let mut title: Option<String> = None;
    let mut body = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            continue;
        }
        if let Some(h) = t.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim().to_string();
            if title.is_none() && !h.is_empty() {
                title = Some(h.clone());
            }
            body.push_str(&h);
        } else {
            // Strip lightweight inline markers.
            let clean = t.replace("**", "").replace("__", "").replace('`', "");
            body.push_str(clean.trim_start_matches(['*', '-', '>', ' ']));
        }
        body.push('\n');
    }
    let fallback = path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled").to_string();
    (title.unwrap_or(fallback), body)
}

fn to_document(raw: &RawFile) -> Result<Document> {
    let canonical = raw.path.to_string_lossy().replace('\\', "/");
    let content = format!("{}\n{}", raw.title, raw.body);
    let metadata_mtime = std::fs::metadata(&raw.path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut metadata = HashMap::new();
    metadata.insert("path".to_string(), canonical.clone());
    Ok(Document {
        id: DocId(fnv1a64(canonical.as_bytes())),
        source: Source::File { path: canonical.clone() },
        url: canonical,
        title: raw.title.clone(),
        headings: Vec::new(),
        body: raw.body.clone(),
        anchor_text: Vec::new(),
        metadata,
        timestamp: metadata_mtime,
        language: "en".to_string(),
        content_hash: fnv1a64_hex(content.as_bytes()),
    })
}

/// Index a corpus directory into one new segment in `data_dir`.
pub fn index_corpus(corpus: &Path, data_dir: &Path, analysis: &AnalysisConfig) -> Result<()> {
    let (files, skipped) = collect_files(corpus)?;
    if files.is_empty() {
        anyhow::bail!("no .txt/.md files under {}", corpus.display());
    }
    let mut index = MemIndex::new(analysis.clone());
    for raw in &files {
        index.add_document(to_document(raw)?);
    }
    let meta = write_segment(data_dir, &index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("indexed {} documents ({} files skipped) -> {}", files.len(), skipped, meta.file_name);
    println!("terms: {}  segment bytes: {}", meta.term_count, meta.bytes);
    Ok(())
}

/// Open every segment in the manifest, verifying checksums on open.
pub fn open_segments(data_dir: &Path) -> Result<Vec<SegmentReader>> {
    let manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    if manifest.segments.is_empty() {
        anyhow::bail!("no segments in {} (run `sealion index <corpus>` first)", data_dir.display());
    }
    let mut readers = Vec::new();
    for name in &manifest.segments {
        let path = data_dir.join(name);
        readers.push(SegmentReader::open(&path).map_err(|e| anyhow::anyhow!("{name}: {e}"))?);
    }
    Ok(readers)
}

/// Search all segments; union of per-segment Boolean results (sorted).
pub fn search_all(
    readers: &[SegmentReader],
    analysis: &AnalysisConfig,
    raw: &str,
) -> Result<Vec<DocId>> {
    let analyzer = Analyzer::new(analysis);
    let query = Query::term_raw(&analyzer, None, raw);
    let mut acc = Vec::new();
    for r in readers {
        let hits = search_view(r, &query).map_err(|e| anyhow::anyhow!("{e}"))?;
        acc = union_sorted(&acc, &hits);
    }
    Ok(acc)
}

/// Print index statistics.
pub fn print_stats(data_dir: &Path) -> Result<()> {
    let manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("generation: {}", manifest.generation);
    println!("segments: {}", manifest.segments.len());
    let mut docs = 0;
    let mut terms = 0;
    let mut bytes = 0u64;
    for name in &manifest.segments {
        let path = data_dir.join(name);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match SegmentReader::open(&path) {
            Ok(r) => {
                println!("  {name}: {} docs, {} terms, {size} bytes", r.len(), r.dictionary().len());
                docs += r.len();
                terms += r.dictionary().len();
                bytes += size;
            }
            Err(e) => println!("  {name}: CORRUPT ({e})"),
        }
    }
    println!("total: {docs} docs, {terms} terms, {bytes} bytes");
    Ok(())
}

/// Verify every segment's checksums; fails if any file is corrupt.
pub fn verify(data_dir: &Path) -> Result<()> {
    let manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    if manifest.segments.is_empty() {
        anyhow::bail!("no segments in {}", data_dir.display());
    }
    let mut bad = 0;
    for name in &manifest.segments {
        let path = data_dir.join(name);
        match SegmentReader::open(&path) {
            Ok(r) => println!("  {name}: OK ({} docs, {} terms)", r.len(), r.dictionary().len()),
            Err(e) => {
                println!("  {name}: CORRUPT ({e})");
                bad += 1;
            }
        }
    }
    if bad > 0 {
        anyhow::bail!("{bad} corrupt segment(s)");
    }
    println!("all {} segment(s) verified", manifest.segments.len());
    Ok(())
}
