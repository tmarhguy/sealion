//! Local corpus ingestion and persistent-index commands (spec §6).
//!
//! Supported: `.txt` (title = file stem), `.md` (title = first `#`
//! heading, else stem), and `.html`/`.htm` (field extraction via the
//! crawler's HTML parser: title/headings/body, `<img>` alt text into the
//! body, image `src`s in `metadata[image_refs]`; binaries never decoded).
//! JSON records and source-code ingestion arrive with later milestones;
//! other files (including image binaries) are counted and skipped honestly.
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
use sealion_index::merge::merge_all_segments;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;
use sealion_index::view::{IndexView, MultiSegmentView};
use sealion_query::execute::search_view;
use sealion_query::query::Query;

/// A harvested corpus file.
struct RawFile {
    path: PathBuf,
    title: String,
    headings: Vec<String>,
    body: String,
    image_refs: Vec<String>,
    outlinks: Vec<String>,
}

/// Recursively collect `.txt`/`.md`/`.html`/`.htm` files, counting skipped
/// formats (binaries, including images, land here).
fn collect_files(root: &Path) -> Result<(Vec<RawFile>, usize)> {
    let mut files = Vec::new();
    let mut skipped = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("cannot list {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read entry in {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                match path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_lowercase())
                {
                    Some(ext) if ext == "txt" => {
                        let body = std::fs::read_to_string(&path)
                            .with_context(|| format!("cannot read {}", path.display()))?;
                        let title = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("untitled")
                            .to_string();
                        files.push(RawFile {
                            path,
                            title,
                            headings: Vec::new(),
                            body,
                            image_refs: Vec::new(),
                            outlinks: Vec::new(),
                        });
                    }
                    Some(ext) if ext == "md" => {
                        let text = std::fs::read_to_string(&path)
                            .with_context(|| format!("cannot read {}", path.display()))?;
                        let (title, body) = parse_markdown(&path, &text);
                        files.push(RawFile {
                            path,
                            title,
                            headings: Vec::new(),
                            body,
                            image_refs: Vec::new(),
                            outlinks: Vec::new(),
                        });
                    }
                    Some(ext) if ext == "html" || ext == "htm" => {
                        let text = std::fs::read_to_string(&path)
                            .with_context(|| format!("cannot read {}", path.display()))?;
                        let base = format!("file://{}", path.to_string_lossy().replace('\\', "/"));
                        let ext = sealion_crawler::html::extract(&base, &text);
                        let mut body = ext.body;
                        if !ext.image_alts.is_empty() {
                            body.push_str("\n\n");
                            body.push_str(&ext.image_alts.join("\n"));
                        }
                        let fallback = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("untitled")
                            .to_string();
                        files.push(RawFile {
                            path,
                            title: if ext.title.is_empty() {
                                fallback
                            } else {
                                ext.title
                            },
                            headings: ext.headings,
                            body,
                            image_refs: ext.image_srcs,
                            outlinks: ext
                                .links
                                .into_iter()
                                .map(|(u, _)| u)
                                // Match stored `url` form (plain paths for
                                // local files) so the link graph connects.
                                .map(|u| u.strip_prefix("file://").unwrap_or(&u).to_string())
                                .take(64)
                                .collect(),
                        });
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
    let fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string();
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
    if !raw.image_refs.is_empty() {
        metadata.insert("image_refs".to_string(), raw.image_refs.join(","));
        metadata.insert("image_count".to_string(), raw.image_refs.len().to_string());
    }
    if !raw.outlinks.is_empty() {
        metadata.insert("outlinks".to_string(), raw.outlinks.join("\n"));
    }
    Ok(Document {
        id: DocId(fnv1a64(canonical.as_bytes())),
        source: Source::File {
            path: canonical.clone(),
        },
        url: canonical,
        title: raw.title.clone(),
        headings: raw.headings.clone(),
        body: raw.body.clone(),
        anchor_text: Vec::new(),
        metadata,
        timestamp: metadata_mtime,
        language: "en".to_string(),
        content_hash: fnv1a64_hex(content.as_bytes()),
    })
}

/// Crawl seed URLs and index the accepted documents as one new segment.
/// Polite by default (robots, delays, domain restriction); loopback fetches
/// are refused unless the config enables them (tests only).
pub async fn crawl_seeds(
    seeds: &[String],
    data_dir: &Path,
    analysis: &AnalysisConfig,
    crawl_cfg: &sealion_core::config::CrawlerConfig,
    state_dir: &Path,
) -> Result<()> {
    let mut crawler = sealion_crawler::Crawler::new(crawl_cfg.clone(), state_dir.to_path_buf())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Resume path: don't re-fetch content already indexed.
    if let Ok(gen) = load_generation(data_dir) {
        let view = MultiSegmentView::new(&gen.readers, &gen.tombstones);
        let hashes: Vec<String> = view
            .all_doc_ids()
            .into_iter()
            .filter_map(|id| view.get(id).map(|d| d.content_hash.clone()))
            .collect();
        crawler.seed_hashes(hashes);
    }
    let accepted = crawler.add_seeds(seeds);
    println!("crawl: {accepted} seed(s) accepted into the frontier");
    let docs = crawler.run().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let stats = crawler.stats().clone();
    if docs.is_empty() {
        println!(
            "crawl: no new documents (failed={} robots_denied={} skipped_content={} exact_dups={} near_dups={})",
            stats.failed, stats.robots_denied, stats.skipped_content, stats.exact_dups, stats.near_dups
        );
        return Ok(());
    }
    let mut index = MemIndex::new(analysis.clone());
    for d in &docs {
        index.add_document(d.clone());
    }
    let meta = write_segment(data_dir, &index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "crawled {} documents -> {} (failed={} robots_denied={} skipped_content={} exact_dups={} near_dups={})",
        docs.len(),
        meta.file_name,
        stats.failed,
        stats.robots_denied,
        stats.skipped_content,
        stats.exact_dups,
        stats.near_dups
    );
    Ok(())
}

/// Index a corpus directory into one new segment in `data_dir`.
pub fn index_corpus(corpus: &Path, data_dir: &Path, analysis: &AnalysisConfig) -> Result<()> {
    let (files, skipped) = collect_files(corpus)?;
    if files.is_empty() {
        anyhow::bail!("no .txt/.md/.html files under {}", corpus.display());
    }
    let mut index = MemIndex::new(analysis.clone());
    for raw in &files {
        index.add_document(to_document(raw)?);
    }
    let meta = write_segment(data_dir, &index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "indexed {} documents ({} files skipped) -> {}",
        files.len(),
        skipped,
        meta.file_name
    );
    println!("terms: {}  segment bytes: {}", meta.term_count, meta.bytes);
    Ok(())
}

/// One live index generation: verified readers plus tombstones.
pub struct Generation {
    pub readers: Vec<SegmentReader>,
    pub tombstones: Vec<u64>,
}

/// Whether `data_dir` holds a cluster layout (`cluster.json`).
pub fn is_cluster(data_dir: &Path) -> bool {
    sealion_distributed::shard::Topology::path(data_dir).exists()
}

/// Create a cluster layout: `shards` shards × `replicas` node copies.
pub fn cluster_init(data_dir: &Path, shards: usize, replicas: usize) -> Result<()> {
    use sealion_distributed::shard::{shard_copy_dir, CopyHealth, ShardCopy, Topology};
    if !(1..=64).contains(&shards) {
        anyhow::bail!("shards must be between 1 and 64");
    }
    if !(1..=8).contains(&replicas) {
        anyhow::bail!("replicas must be between 1 and 8");
    }
    if is_cluster(data_dir) {
        anyhow::bail!("cluster already initialized in {}", data_dir.display());
    }
    let nodes: Vec<String> = (0..replicas).map(|i| format!("node-{i}")).collect();
    let mut copies = Vec::new();
    for s in 0..shards {
        // Round-robin placement: copy j of shard s on node (s+j) % R.
        for j in 0..replicas {
            let node = nodes[(s + j) % nodes.len()].clone();
            let dir = shard_copy_dir(data_dir, &node, s);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            // Seed an empty manifest so opens succeed pre-index.
            let manifest = Manifest::load_or_new(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
            manifest.store(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
            copies.push(ShardCopy {
                shard: s,
                node,
                generation: 0,
                health: CopyHealth::Healthy,
                last_heartbeat_ms: 0,
            });
        }
    }
    Topology {
        shard_count: shards,
        nodes,
        copies,
    }
    .store(data_dir)
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "cluster: {shards} shards × {replicas} replicas initialized in {}",
        data_dir.display()
    );
    Ok(())
}

/// Print cluster status: shards, copies, health, generations, doc counts.
pub fn cluster_status(data_dir: &Path) -> Result<()> {
    use sealion_distributed::shard::Topology;
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "cluster: {} shards, {} nodes, {} copies",
        topo.shard_count,
        topo.nodes.len(),
        topo.copies.len()
    );
    for s in 0..topo.shard_count {
        let copies: Vec<_> = topo.copies.iter().filter(|c| c.shard == s).collect();
        let healthy = copies
            .iter()
            .filter(|c| c.health == sealion_distributed::CopyHealth::Healthy)
            .count();
        // Doc count from the first openable copy.
        let mut docs = String::from("?");
        for c in &copies {
            let dir = sealion_distributed::shard::shard_copy_dir(data_dir, &c.node, s);
            if let Ok(m) = Manifest::load_or_new(&dir) {
                let n: usize = m.segments.len();
                // Sum live docs across segments best-effort.
                let mut live = 0;
                for name in &m.segments {
                    if let Ok(r) = SegmentReader::open(&dir.join(name)) {
                        live += r.len();
                    }
                }
                docs = format!("{live} docs in {n} segs");
                break;
            }
        }
        let state = if healthy == 0 {
            "OFFLINE"
        } else if healthy < copies.len() {
            "DEGRADED"
        } else {
            "healthy"
        };
        println!("  shard {s:02} [{state}]: {docs}");
        for c in copies {
            println!("    {} gen={} {:?}", c.node, c.generation, c.health);
        }
    }
    let missing = topo.unavailable_shards();
    if !missing.is_empty() {
        println!("  UNAVAILABLE shards: {missing:?} (searches will be partial)");
    }
    Ok(())
}

/// List shards with per-shard statistics.
pub fn shard_list(data_dir: &Path) -> Result<()> {
    cluster_status(data_dir)
}

/// List nodes with hosted copies and health.
pub fn node_list(data_dir: &Path) -> Result<()> {
    use sealion_distributed::shard::Topology;
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    for node in &topo.nodes {
        let hosted: Vec<_> = topo.copies.iter().filter(|c| &c.node == node).collect();
        let bad = hosted
            .iter()
            .filter(|c| c.health != sealion_distributed::CopyHealth::Healthy)
            .count();
        println!("  {node}: {} copies ({} unhealthy)", hosted.len(), bad);
        for c in hosted {
            println!(
                "    shard {:02} gen={} {:?}",
                c.shard, c.generation, c.health
            );
        }
    }
    Ok(())
}

/// Move one shard copy (online rebalance).
pub fn cluster_move(data_dir: &Path, shard: usize, dest: &str, from: Option<&str>) -> Result<()> {
    use sealion_distributed::shard::Topology;
    let mut topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    if shard >= topo.shard_count {
        anyhow::bail!("no shard {shard} (have {})", topo.shard_count);
    }
    let src = match from {
        Some(n) => n.to_string(),
        None => topo
            .copies
            .iter()
            .find(|c| c.shard == shard)
            .map(|c| c.node.clone())
            .ok_or_else(|| anyhow::anyhow!("shard {shard:02} has no copies"))?,
    };
    let copied = sealion_distributed::rebalance::move_shard(data_dir, &mut topo, shard, &src, dest)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "moved shard {shard:02} {src} -> {dest} ({} files, queries uninterrupted)",
        copied.len()
    );
    Ok(())
}

/// Index a corpus into a cluster: partition docs by shard, write one
/// segment per shard, replicate to all healthy copies (verified).
pub fn index_corpus_clustered(
    corpus: &Path,
    data_dir: &Path,
    analysis: &AnalysisConfig,
) -> Result<()> {
    use sealion_distributed::replica::copy_segments_verified;
    use sealion_distributed::shard::{shard_copy_dir, shard_of, Topology};
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let (files, skipped) = collect_files(corpus)?;
    if files.is_empty() {
        anyhow::bail!("no .txt/.md/.html files under {}", corpus.display());
    }
    // Partition (stable assignment; re-indexing replaces same IDs).
    let mut per_shard: std::collections::HashMap<usize, MemIndex> =
        std::collections::HashMap::new();
    for raw in &files {
        let doc = to_document(raw)?;
        let s = shard_of(doc.id, topo.shard_count);
        per_shard
            .entry(s)
            .or_insert_with(|| MemIndex::new(analysis.clone()))
            .add_document(doc);
    }
    // Primaries: first healthy copy per shard (insertion order).
    let mut primaries: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for c in &topo.copies {
        if c.health == sealion_distributed::CopyHealth::Healthy {
            primaries.entry(c.shard).or_insert_with(|| c.node.clone());
        }
    }
    let mut total = 0;
    for (s, index) in &per_shard {
        let Some(node) = primaries.get(s) else {
            anyhow::bail!("shard {s:02} has no healthy copy; refusing partial index");
        };
        let dir = shard_copy_dir(data_dir, node, *s);
        let meta = write_segment(&dir, index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
        total += index.len();
        // Replicate the new segment to sibling copies.
        for c in topo
            .copies
            .iter()
            .filter(|c| c.shard == *s && &c.node != node)
        {
            let dst = shard_copy_dir(data_dir, &c.node, *s);
            if let Err(e) = copy_segments_verified(&dir, &dst) {
                eprintln!("warning: replica {} shard {s:02} failed: {e}", c.node);
            }
        }
        let _ = meta;
    }
    println!(
        "indexed {total} documents ({skipped} files skipped) across {} shards",
        per_shard.len()
    );
    Ok(())
}

/// Open every segment in the manifest, verifying checksums on open.
/// Kept for API compat; prefer [`load_generation`] for tombstone-aware work.
#[allow(dead_code)]
pub fn open_segments(data_dir: &Path) -> Result<Vec<SegmentReader>> {
    Ok(load_generation(data_dir)?.readers)
}

/// Load readers + tombstones for generation-aware search/merge.
pub fn load_generation(data_dir: &Path) -> Result<Generation> {
    let manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    if manifest.segments.is_empty() {
        anyhow::bail!(
            "no segments in {} (run `sealion index <corpus>` first)",
            data_dir.display()
        );
    }
    let mut readers = Vec::new();
    for name in &manifest.segments {
        let path = data_dir.join(name);
        readers.push(SegmentReader::open(&path).map_err(|e| anyhow::anyhow!("{name}: {e}"))?);
    }
    Ok(Generation {
        readers,
        tombstones: manifest.deleted,
    })
}

/// Search all segments with generation semantics: newer shadows older,
/// tombstones invisible. Returns sorted live DocIds.
/// Kept for API compat (no tombstones); prefer [`search_all_with_tombstones`].
#[allow(dead_code)]
pub fn search_all(
    readers: &[SegmentReader],
    analysis: &AnalysisConfig,
    raw: &str,
) -> Result<Vec<DocId>> {
    search_all_with_tombstones(readers, &[], analysis, raw)
}

pub fn search_all_with_tombstones(
    readers: &[SegmentReader],
    tombstones: &[u64],
    analysis: &AnalysisConfig,
    raw: &str,
) -> Result<Vec<DocId>> {
    let analyzer = Analyzer::new(analysis);
    let query = Query::term_raw(&analyzer, None, raw);
    let view = MultiSegmentView::new(readers, tombstones);
    search_view(&view, &query).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolve stored docs with newest-wins semantics for display.
pub fn resolve_docs(
    readers: &[SegmentReader],
    tombstones: &[u64],
) -> std::collections::HashMap<DocId, sealion_core::document::Document> {
    let view = MultiSegmentView::new(readers, tombstones);
    let mut out = std::collections::HashMap::new();
    for id in view.all_doc_ids() {
        if let Some(d) = view.get(id) {
            out.insert(id, d.clone());
        }
    }
    out
}

/// Merge every shard in a cluster, replicating compacted segments.
pub fn merge_cluster(data_dir: &Path, analysis: &AnalysisConfig) -> Result<()> {
    use sealion_distributed::replica::copy_segments_verified;
    use sealion_distributed::shard::{shard_copy_dir, Topology};
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    for s in 0..topo.shard_count {
        let primary = topo
            .healthy_copies(s)
            .into_iter()
            .next()
            .map(|c| c.node.clone());
        let Some(node) = primary else {
            eprintln!("shard {s:02}: no healthy copy, skipping");
            continue;
        };
        let dir = shard_copy_dir(data_dir, &node, s);
        match sealion_index::merge::merge_all_segments(&dir, analysis) {
            Ok(name) => {
                println!("shard {s:02}: merged -> {name}");
                for c in topo
                    .copies
                    .iter()
                    .filter(|c| c.shard == s && c.node != node)
                {
                    let dst = shard_copy_dir(data_dir, &c.node, s);
                    if let Err(e) = copy_segments_verified(&dir, &dst) {
                        eprintln!("warning: replica {} shard {s:02}: {e}", c.node);
                    }
                }
            }
            Err(e) => println!("shard {s:02}: nothing to merge ({e})"),
        }
    }
    Ok(())
}

/// Tombstone one document in its owning shard (cluster mode).
pub fn delete_clustered(data_dir: &Path, id: u64) -> Result<()> {
    use sealion_distributed::replica::copy_segments_verified;
    use sealion_distributed::shard::{shard_copy_dir, shard_of, Topology};
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let s = shard_of(DocId(id), topo.shard_count);
    let primary = topo
        .healthy_copies(s)
        .into_iter()
        .next()
        .map(|c| c.node.clone())
        .ok_or_else(|| anyhow::anyhow!("shard {s:02} has no healthy copy"))?;
    let dir = shard_copy_dir(data_dir, &primary, s);
    let mut manifest = Manifest::load_or_new(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    manifest.add_tombstone(id);
    manifest.store(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    for c in topo
        .copies
        .iter()
        .filter(|c| c.shard == s && c.node != primary)
    {
        let dst = shard_copy_dir(data_dir, &c.node, s);
        let _ = copy_segments_verified(&dir, &dst);
    }
    println!("deleted DocId({id}) from shard {s:02} (tombstoned)");
    Ok(())
}

/// Compute PageRank authority over the corpus link graph and publish it
/// as a new generation (same DocIds, `metadata["authority"]` set).
/// Newest-wins shadowing makes the scores live immediately; merge later
/// to compact. Prints the top authorities.
pub fn index_authority(
    data_dir: &Path,
    analysis: &AnalysisConfig,
    damping: f64,
    iterations: usize,
) -> Result<()> {
    if !(0.0..1.0).contains(&damping) {
        anyhow::bail!("damping must be in (0, 1)");
    }
    let iterations = iterations.clamp(1, 1000);
    if is_cluster(data_dir) {
        return index_authority_clustered(data_dir, analysis, damping, iterations);
    }
    let gen = load_generation(data_dir)?;
    let docs = resolve_docs(&gen.readers, &gen.tombstones);
    let mut all: Vec<_> = docs.into_values().collect();
    all.sort_by_key(|d| d.id);
    publish_authority(data_dir, analysis, &all, damping, iterations, None)
}

fn publish_authority(
    data_dir: &Path,
    analysis: &AnalysisConfig,
    docs: &[sealion_core::document::Document],
    damping: f64,
    iterations: usize,
    shard_of: Option<&dyn Fn(DocId) -> usize>,
) -> Result<()> {
    use sealion_index::authority::{build_graph, pagerank};
    let graph = build_graph(docs);
    let edges: usize = graph.values().map(Vec::len).sum();
    let scores = pagerank(&graph, damping, iterations);
    // Partition by shard when clustered (global graph, per-shard segments).
    let mut per_shard: std::collections::HashMap<usize, MemIndex> =
        std::collections::HashMap::new();
    let mut single: Option<MemIndex> = None;
    for mut d in docs.iter().cloned() {
        let s = scores.get(&d.id).copied().unwrap_or(0.0);
        d.metadata
            .insert("authority".to_string(), format!("{s:.6}"));
        match shard_of {
            Some(f) => {
                per_shard
                    .entry(f(d.id))
                    .or_insert_with(|| MemIndex::new(analysis.clone()))
                    .add_document(d);
            }
            None => {
                single
                    .get_or_insert_with(|| MemIndex::new(analysis.clone()))
                    .add_document(d);
            }
        }
    }
    if let Some(index) = single {
        let meta = write_segment(data_dir, &index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "authority: {} docs, {} internal edges -> {}",
            docs.len(),
            edges,
            meta.file_name
        );
    } else {
        use sealion_distributed::replica::copy_segments_verified;
        use sealion_distributed::shard::{shard_copy_dir, Topology};
        let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
        for (s, index) in &per_shard {
            let primary = topo
                .healthy_copies(*s)
                .into_iter()
                .next()
                .map(|c| c.node.clone())
                .ok_or_else(|| anyhow::anyhow!("shard {s:02} has no healthy copy"))?;
            let dir = shard_copy_dir(data_dir, &primary, *s);
            write_segment(&dir, index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
            for c in topo
                .copies
                .iter()
                .filter(|c| c.shard == *s && c.node != primary)
            {
                let dst = shard_copy_dir(data_dir, &c.node, *s);
                if let Err(e) = copy_segments_verified(&dir, &dst) {
                    eprintln!("warning: replica {} shard {s:02}: {e}", c.node);
                }
            }
        }
        println!(
            "authority: {} docs, {} internal edges across {} shards",
            docs.len(),
            edges,
            per_shard.len()
        );
    }
    // Top authorities preview.
    let mut top: Vec<(DocId, f64)> = scores.into_iter().collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (id, s) in top.into_iter().take(5) {
        println!("  [{id:?}] authority={s:.4}");
    }
    Ok(())
}

fn index_authority_clustered(
    data_dir: &Path,
    analysis: &AnalysisConfig,
    damping: f64,
    iterations: usize,
) -> Result<()> {
    use sealion_distributed::shard::{shard_copy_dir, shard_of, Topology};
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Global doc set across all servable shards (global graph, §54).
    let mut all = Vec::new();
    for s in 0..topo.shard_count {
        let Some(copy) = topo.healthy_copies(s).into_iter().next() else {
            continue;
        };
        let dir = shard_copy_dir(data_dir, &copy.node, s);
        let Ok(manifest) = Manifest::load_or_new(&dir) else {
            continue;
        };
        let mut readers = Vec::new();
        for name in &manifest.segments {
            if let Ok(r) = SegmentReader::open(&dir.join(name)) {
                readers.push(r);
            }
        }
        let view = MultiSegmentView::new(&readers, &manifest.deleted);
        for id in view.all_doc_ids() {
            if let Some(d) = view.get(id) {
                all.push(d.clone());
            }
        }
    }
    all.sort_by_key(|d| d.id);
    let count = topo.shard_count;
    publish_authority(
        data_dir,
        analysis,
        &all,
        damping,
        iterations,
        Some(&move |id| shard_of(id, count)),
    )
}

/// Tombstone one document without rebuilding. Takes effect immediately;
/// a later merge drops it permanently.
pub fn delete_document(data_dir: &Path, id: u64) -> Result<()> {
    let mut manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    if manifest.segments.is_empty() {
        anyhow::bail!("no segments in {}", data_dir.display());
    }
    manifest.add_tombstone(id);
    manifest
        .store(data_dir)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "deleted DocId({id}) (tombstoned, generation {})",
        manifest.generation
    );
    Ok(())
}

/// Merge all segments into one, reclaiming updated/deleted docs.
pub fn merge_segments(data_dir: &Path, analysis: &AnalysisConfig) -> Result<()> {
    let name = merge_all_segments(data_dir, analysis).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("merged -> {name}");
    Ok(())
}

/// Cluster search: coordinator fan-out with global stats, global merge,
/// and `partial` semantics (§62–69). Display matches single-node search.
pub async fn cluster_search(
    data_dir: &Path,
    cfg: &sealion_core::config::Config,
    raw: &str,
    explain: bool,
) -> Result<()> {
    use sealion_distributed::replica::Router;
    use sealion_distributed::shard::Topology;
    let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let query = sealion_query::parser::parse(&cfg.analysis, cfg.query.max_query_terms, raw)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut router = Router::default();
    let res = sealion_distributed::distributed_search(
        data_dir,
        &topo,
        &mut router,
        &cfg.analysis,
        &cfg.ranking,
        &query,
        cfg.query.top_k,
    )
    .await;
    println!(
        "{} result(s) in {:.2}ms (partial: {}, distributed {} shards)",
        res.hits.len(),
        res.took_ms,
        res.partial,
        res.per_shard_ms.len(),
    );
    for d in &res.diagnostics {
        println!("  ! {d}");
    }
    if explain {
        for (shard, ms) in &res.per_shard_ms {
            println!("  shard {shard:02}: {ms:.2}ms");
        }
        for h in &res.hits {
            let title = res
                .docs
                .get(&h.doc)
                .map(|dd| dd.title.as_str())
                .unwrap_or("<missing>");
            println!("  [{}] score={:.4} {}", h.doc.0, h.score, title);
            for t in &h.explain.terms {
                println!(
                    "      {:?}:{} tf={} df={} idf={:.3} len={} avg={:.1} -> {:.4}",
                    t.field, t.term, t.tf, t.df, t.idf, t.doc_len, t.avg_len, t.contrib
                );
            }
            if h.explain.authority != 0.0 || h.explain.freshness != 0.0 {
                println!(
                    "      authority={:.4} freshness={:.4}",
                    h.explain.authority, h.explain.freshness
                );
            }
        }
        return Ok(());
    }
    // Snippets need expanded terms; rebuild a scratch view over primaries.
    // (Coordinator already resolved docs; expansion reuses any shard vocab
    // via the first available view — expansions are global by construction.)
    let terms = query.terms();
    for h in &res.hits {
        match res.docs.get(&h.doc) {
            Some(d) => {
                let snip = sealion_query::rank::make_snippet(d, &terms, &cfg.analysis, 180)
                    .replace("<mark>", "**")
                    .replace("</mark>", "**")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&")
                    .replace("&quot;", "\"");
                println!("  [{}] {:.4} {} -- {}", h.doc.0, h.score, d.title, d.url);
                println!("      {snip}");
            }
            None => println!("  [{}] <missing>", h.doc.0),
        }
    }
    Ok(())
}

/// Query benchmark (§88): the §86 mix over the live index, Block-Max WAND
/// path, with optional query-result cache to justify it (§72).
pub fn bench_query(
    data_dir: &Path,
    cfg: &sealion_core::config::Config,
    rounds: usize,
    use_cache: bool,
    exhaustive: bool,
) -> Result<()> {
    let manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    if manifest.segments.is_empty() {
        anyhow::bail!("no segments in {}", data_dir.display());
    }
    let gen = load_generation(data_dir)?;
    let view = MultiSegmentView::new(&gen.readers, &gen.tombstones);
    let queries: Vec<String> = sealion_bench::query_mix()
        .into_iter()
        .map(str::to_string)
        .collect();
    let rounds = rounds.clamp(1, 25);
    // Uncached baseline first, then cached (same process, same view).
    let plain = sealion_bench::run_query_bench(sealion_bench::QueryBenchInput {
        view: &view,
        analysis: &cfg.analysis,
        ranking: &cfg.ranking,
        queries: &queries,
        top_k: cfg.query.top_k,
        max_terms: cfg.query.max_query_terms,
        generation: manifest.generation,
        rounds,
        cache: None,
        exhaustive,
    });
    print_query_bench(
        if exhaustive {
            "exhaustive"
        } else {
            "block-max-wand"
        },
        &plain,
    );
    if use_cache {
        let mut cache = sealion_query::cache::QueryCache::new(256);
        let cached = sealion_bench::run_query_bench(sealion_bench::QueryBenchInput {
            view: &view,
            analysis: &cfg.analysis,
            ranking: &cfg.ranking,
            queries: &queries,
            top_k: cfg.query.top_k,
            max_terms: cfg.query.max_query_terms,
            generation: manifest.generation,
            rounds,
            cache: Some(&mut cache),
            exhaustive,
        });
        print_query_bench("cached", &cached);
    }
    Ok(())
}

fn print_query_bench(label: &str, b: &sealion_bench::QueryBench) {
    println!(
        "bench query [{label}]: {} queries × {} rounds  qps={:.0}  mean={:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms  scored/query={:.1} skipped/query={:.1}  cache_hit={:.2}",
        b.queries / b.rounds,
        b.rounds,
        b.qps,
        b.latency.mean_ms,
        b.latency.p50_ms,
        b.latency.p95_ms,
        b.latency.p99_ms,
        b.wand_scored_avg,
        b.wand_skipped_avg,
        b.cache_hit_rate
    );
}

/// Indexing benchmark (§87): build a temp segment from a corpus directory.
pub fn bench_index(corpus: &Path, analysis: &AnalysisConfig) -> Result<()> {
    let t0 = std::time::Instant::now();
    let (files, skipped) = collect_files(corpus)?;
    if files.is_empty() {
        anyhow::bail!("no indexable files under {}", corpus.display());
    }
    let bytes_in: u64 = files
        .iter()
        .map(|f| std::fs::metadata(&f.path).map(|m| m.len()).unwrap_or(0))
        .sum();
    let mut index = MemIndex::new(analysis.clone());
    for raw in &files {
        index.add_document(to_document(raw)?);
    }
    let postings = index.posting_count();
    let terms = index.term_count();
    let tokens: u64 = sealion_core::field::Field::ALL
        .iter()
        .map(|f| index.field_token_total(*f))
        .sum();
    let tmp = std::env::temp_dir().join(format!("sealion-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let meta = write_segment(&tmp, &index, true).map_err(|e| anyhow::anyhow!("{e}"))?;
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "bench index: {} docs ({} skipped) in {:.2}s  {:.0} docs/s  {:.1} MB/s  {} terms  {} postings  {:.0} tokens/s  {} segment bytes ({:.1}% of input)",
        files.len(),
        skipped,
        secs,
        files.len() as f64 / secs.max(1e-9),
        bytes_in as f64 / 1e6 / secs.max(1e-9),
        terms,
        postings,
        tokens as f64 / secs.max(1e-9),
        meta.bytes,
        100.0 * meta.bytes as f64 / bytes_in.max(1) as f64
    );
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Distributed scaling + failure benchmark (§91–92).
///
/// For each shard count: build the layout in a temp dir (timed indexing),
/// then run the query battery (single-node path for N=1, coordinator
/// fan-out otherwise). Afterwards, on a 4-shard ×2-replica layout:
/// single-copy loss (failover cost, still complete), full-shard loss
/// (partial + coverage), and a live shard move (results unchanged).
pub async fn bench_scale(
    corpus: &Path,
    cfg: &sealion_core::config::Config,
    shards: &[usize],
    rounds: usize,
) -> Result<()> {
    use sealion_distributed::replica::Router;
    use sealion_distributed::shard::{shard_copy_dir, Topology};

    let queries: Vec<String> = sealion_bench::query_mix()
        .into_iter()
        .map(str::to_string)
        .collect();
    let rounds = rounds.clamp(1, 10);
    println!(
        "scale: {} queries × {} rounds | corpus {}",
        queries.len(),
        rounds,
        corpus.display()
    );
    println!("shards | index docs/s | qps | p50 ms | p99 ms | partial");

    let mut scale_dirs = Vec::new();
    for &n in shards {
        let dir = std::env::temp_dir().join(format!("sealion-scale-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t0 = std::time::Instant::now();
        if n <= 1 {
            index_corpus(corpus, &dir, &cfg.analysis)?;
        } else {
            cluster_init(&dir, n, 1)?;
            index_corpus_clustered(corpus, &dir, &cfg.analysis)?;
        }
        let index_secs = t0.elapsed().as_secs_f64();
        // Count docs for the docs/s figure.
        let docs = count_docs(&dir)?;
        let docs_per_sec = docs as f64 / index_secs.max(1e-9);

        // Search battery.
        let mut samples = Vec::new();
        let mut partial_seen = false;
        for _ in 0..rounds {
            for raw in &queries {
                let query =
                    sealion_query::parser::parse(&cfg.analysis, cfg.query.max_query_terms, raw)
                        .unwrap_or(sealion_query::query::Query::MatchNothing);
                if n <= 1 {
                    let gen = load_generation(&dir)?;
                    let view = MultiSegmentView::new(&gen.readers, &gen.tombstones);
                    let t = std::time::Instant::now();
                    let _ = sealion_query::wand::block_max_wand_search(
                        &view,
                        &cfg.analysis,
                        &cfg.ranking,
                        &query,
                        cfg.query.top_k,
                    );
                    samples.push(t.elapsed().as_secs_f64() * 1000.0);
                } else {
                    let topo = Topology::load(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
                    let mut router = Router::default();
                    let res = sealion_distributed::distributed_search(
                        &dir,
                        &topo,
                        &mut router,
                        &cfg.analysis,
                        &cfg.ranking,
                        &query,
                        cfg.query.top_k,
                    )
                    .await;
                    partial_seen |= res.partial;
                    samples.push(res.took_ms);
                }
            }
        }
        let lat = sealion_bench::Latency::from_ms(samples);
        let total_s: f64 = (lat.mean_ms * lat.runs as f64) / 1000.0;
        println!(
            "{n:>6} | {docs_per_sec:>11.0} | {:>3.0} | {:>6.2} | {:>6.2} | {partial_seen}",
            lat.runs as f64 / total_s.max(1e-9),
            lat.p50_ms,
            lat.p99_ms,
        );
        scale_dirs.push(dir);
    }

    // Failure benchmark (§92) on a dedicated 4-shard ×2 layout.
    let fdir = std::env::temp_dir().join(format!("sealion-scale-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&fdir);
    cluster_init(&fdir, 4, 2)?;
    index_corpus_clustered(corpus, &fdir, &cfg.analysis)?;
    let probe = "compiler";
    let query = sealion_query::parser::parse(&cfg.analysis, cfg.query.max_query_terms, probe)
        .unwrap_or(sealion_query::query::Query::MatchNothing);

    async fn probe_search(
        dir: &std::path::Path,
        cfg: &sealion_core::config::Config,
        query: &sealion_query::query::Query,
    ) -> (f64, bool, Vec<u64>) {
        let topo = Topology::load(dir).expect("topology");
        let mut router = Router::default();
        let res = sealion_distributed::distributed_search(
            dir,
            &topo,
            &mut router,
            &cfg.analysis,
            &cfg.ranking,
            query,
            cfg.query.top_k,
        )
        .await;
        (
            res.took_ms,
            res.partial,
            res.hits.iter().map(|h| h.doc.0).collect(),
        )
    }

    let (base_ms, base_partial, base_hits) = probe_search(&fdir, cfg, &query).await;
    println!(
        "failover baseline: {base_ms:.2}ms partial={base_partial} hits={}",
        base_hits.len()
    );

    // Kill ONE copy of shard 0: traffic must shift, results complete.
    let victim = shard_copy_dir(&fdir, "node-0", 0);
    let victim_exists = victim.exists();
    if victim_exists {
        std::fs::remove_dir_all(&victim).ok();
    }
    let (ms, partial, hits) = probe_search(&fdir, cfg, &query).await;
    println!(
        "failover 1 copy lost: {ms:.2}ms partial={partial} hits={} (complete={})",
        hits.len(),
        !partial && hits == base_hits
    );

    // Kill ALL copies of shard 1: partial, survivors served.
    for node in ["node-0", "node-1"] {
        std::fs::remove_dir_all(shard_copy_dir(&fdir, node, 1)).ok();
    }
    let (ms, partial, hits) = probe_search(&fdir, cfg, &query).await;
    println!(
        "failover shard lost: {ms:.2}ms partial={partial} hits={}",
        hits.len()
    );

    // Restore shard 1 by reindexing, then move shard 2 live.
    index_corpus_clustered(corpus, &fdir, &cfg.analysis)?;
    let (_, _, pre_move) = probe_search(&fdir, cfg, &query).await;
    cluster_move(&fdir, 2, "node-9", None)?;
    let (ms, partial, post_move) = probe_search(&fdir, cfg, &query).await;
    println!(
        "migration move shard 02: {ms:.2}ms partial={partial} identical={}",
        pre_move == post_move
    );

    for dir in scale_dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
    let _ = std::fs::remove_dir_all(fdir);
    Ok(())
}

/// Live document count across single or cluster layouts (for docs/s).
fn count_docs(data_dir: &Path) -> Result<usize> {
    if is_cluster(data_dir) {
        use sealion_distributed::shard::{shard_copy_dir, Topology};
        let topo = Topology::load(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut total = 0;
        for s in 0..topo.shard_count {
            // First servable copy only: replicas hold identical doc sets.
            if let Some(c) = topo.healthy_copies(s).into_iter().next() {
                let dir = shard_copy_dir(data_dir, &c.node, s);
                if let Ok(manifest) = Manifest::load_or_new(&dir) {
                    for name in &manifest.segments {
                        if let Ok(r) = SegmentReader::open(&dir.join(name)) {
                            total += r.len();
                        }
                    }
                }
            }
        }
        Ok(total)
    } else {
        Ok(load_generation(data_dir)?
            .readers
            .iter()
            .map(|r| r.len())
            .sum())
    }
}

/// Run the relevance harness (§51–53) over `<eval_dir>/{corpus,queries,judgments}`.
///
/// Self-contained: indexes the eval corpus into a temp segment, resolves
/// judgment basenames to live DocIds, scores every query with exhaustive
/// BM25 (the §41 baseline both engines must equal), and prints means plus
/// per-query nDCG@10. `--save` writes the JSON report; `--baseline`
/// compares against a saved report (§53 regression format).
pub fn eval_relevance(
    eval_dir: &Path,
    cfg: &sealion_core::config::Config,
    save: Option<&str>,
    baseline: Option<&str>,
) -> Result<()> {
    use std::collections::{BTreeMap, HashMap};

    let corpus = eval_dir.join("corpus");
    let queries_tsv =
        std::fs::read_to_string(eval_dir.join("queries/queries.tsv")).with_context(|| {
            format!(
                "cannot read {}",
                eval_dir.join("queries/queries.tsv").display()
            )
        })?;
    let judgments_tsv = std::fs::read_to_string(eval_dir.join("judgments/judgments.tsv"))
        .with_context(|| {
            format!(
                "cannot read {}",
                eval_dir.join("judgments/judgments.tsv").display()
            )
        })?;
    let queries = sealion_eval::load_queries(&queries_tsv).map_err(|e| anyhow::anyhow!("{e}"))?;
    let qrels = sealion_eval::load_judgments(&judgments_tsv).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Index the eval corpus in isolation (never touches the user index).
    let tmp = std::env::temp_dir().join(format!("sealion-eval-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    index_corpus(&corpus, &tmp, &cfg.analysis)?;
    let gen = load_generation(&tmp)?;
    let view = MultiSegmentView::new(&gen.readers, &gen.tombstones);

    // Basename → DocId through stored URLs.
    let mut by_base: HashMap<String, DocId> = HashMap::new();
    for id in view.all_doc_ids() {
        if let Some(d) = view.get(id) {
            let base = d.url.rsplit('/').next().unwrap_or(&d.url).to_string();
            by_base.insert(base, id);
        }
    }
    let mut ranked: BTreeMap<String, Vec<DocId>> = BTreeMap::new();
    let mut rel: BTreeMap<String, HashMap<DocId, u8>> = BTreeMap::new();
    for q in &queries {
        let query = sealion_query::parser::parse(&cfg.analysis, cfg.query.max_query_terms, &q.text)
            .map_err(|e| anyhow::anyhow!("query {}: {e}", q.id))?;
        let hits =
            sealion_query::rank::ranked_search(&view, &cfg.analysis, &cfg.ranking, &query, 10)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        ranked.insert(q.id.clone(), hits.into_iter().map(|h| h.doc).collect());
        let mut grades = HashMap::new();
        for (base, id) in &by_base {
            let g = qrels.grade(&q.id, base);
            if g > 0 {
                grades.insert(*id, g);
            }
        }
        rel.insert(q.id.clone(), grades);
    }
    let report = sealion_eval::evaluate(&queries, &ranked, &rel);
    println!(
        "relevance: {} queries  nDCG@10={:.4}  MRR={:.4}  MAP={:.4}  P@10={:.4}  R@10={:.4}",
        report.queries.len(),
        report.mean_ndcg_at_10,
        report.mean_mrr,
        report.mean_ap,
        report.mean_precision_at_10,
        report.mean_recall_at_10
    );
    for row in &report.queries {
        println!(
            "  {:<6} nDCG@10={:.4} MRR={:.4} AP={:.4} rel={} got={}  {}",
            row.query_id,
            row.ndcg_at_10,
            row.mrr,
            row.ap,
            row.relevant,
            row.retrieved,
            row.query_text
        );
    }
    if let Some(path) = save {
        let json = serde_json::to_string_pretty(&report).map_err(|e| anyhow::anyhow!("{e}"))?;
        std::fs::write(path, json).with_context(|| format!("cannot write {path}"))?;
        println!("saved report -> {path}");
    }
    if let Some(path) = baseline {
        let bytes = std::fs::read(path).with_context(|| format!("cannot read {path}"))?;
        let old: sealion_eval::Report =
            serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
        let reg = sealion_eval::compare_reports(&old, &report);
        println!(
            "regression vs {path}: ΔnDCG@10={:+.4} ΔMRR={:+.4} ΔMAP={:+.4}  improved={} regressed={} unchanged={}",
            reg.delta_ndcg_at_10,
            reg.delta_mrr,
            reg.delta_ap,
            reg.queries_improved,
            reg.queries_regressed,
            reg.queries_unchanged
        );
        for (qid, o, n) in &reg.largest_regressions {
            println!("  REGRESSED {qid}: nDCG@10 {o:.4} -> {n:.4}");
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Print index statistics.
pub fn print_stats(data_dir: &Path) -> Result<()> {
    let manifest = Manifest::load_or_new(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("generation: {}", manifest.generation);
    println!("segments: {}", manifest.segments.len());
    println!("tombstones: {}", manifest.deleted.len());
    let mut docs = 0;
    let mut terms = 0;
    let mut bytes = 0u64;
    for name in &manifest.segments {
        let path = data_dir.join(name);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match SegmentReader::open(&path) {
            Ok(r) => {
                println!(
                    "  {name}: {} docs, {} terms, {size} bytes",
                    r.len(),
                    r.dictionary().len()
                );
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
            Ok(r) => println!(
                "  {name}: OK ({} docs, {} terms)",
                r.len(),
                r.dictionary().len()
            ),
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
