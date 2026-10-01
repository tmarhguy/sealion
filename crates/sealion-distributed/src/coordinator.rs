//! Query coordinator (spec §62–65): parse → plan → parallel fan-out →
//! local top-N → global merge → final ranking.
//!
//! Exactness design (§63–64): the coordinator first gathers global
//! statistics (doc count, per-field totals, per-term df across all live
//! shards), then every shard scores its Boolean candidates under those
//! *global* statistics. Scores are therefore comparable across shards and
//! the merged top-k equals a single-index run exactly (property-tested).
//!
//! Shards that cannot serve (no healthy copy) are skipped with
//! `partial = true` and a diagnostic — never silent (§69).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sealion_core::config::{AnalysisConfig, RankingConfig};
use sealion_core::document::{DocId, Document};
use sealion_core::field::Field;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::view::{IndexView, MultiSegmentView};
use sealion_query::query::Query;
use sealion_query::rank::{expand_terms_from_vocab, ranked_search_with_stats, RankedHit};

use crate::replica::Router;
use crate::shard::{shard_copy_dir, GlobalStats, Topology};

/// One shard's opened generation (primary copy selected by the router).
pub struct OpenShard {
    pub shard: usize,
    pub dir: PathBuf,
    pub readers: Vec<SegmentReader>,
    pub tombstones: Vec<u64>,
}

/// Coordinator search result.
#[derive(Debug)]
pub struct CoordinatorResult {
    pub hits: Vec<RankedHit>,
    pub docs: HashMap<DocId, Document>,
    pub partial: bool,
    pub diagnostics: Vec<String>,
    pub per_shard_ms: Vec<(usize, f64)>,
    pub took_ms: f64,
}

/// Open every servable shard's primary copy. Unavailable shards are
/// reported (not failed) for `partial` semantics.
pub fn open_shards(
    data_dir: &Path,
    topo: &Topology,
    router: &mut Router,
) -> (Vec<OpenShard>, Vec<String>) {
    let mut shards = Vec::new();
    let mut diagnostics = Vec::new();
    for shard in 0..topo.shard_count {
        // Try every healthy copy in turn (replica failover, §66): traffic
        // shifts on failure, and only total loss yields partial results.
        // The router picks first (round-robin / least-outstanding, §67).
        let mut order: Vec<(String, bool)> = Vec::new(); // (node, router-picked)
        if let Some(first) = router.pick(topo, shard) {
            order.push((first.node, true));
        }
        for copy in topo.healthy_copies(shard) {
            if order.iter().any(|(n, _)| n == &copy.node) {
                continue;
            }
            order.push((copy.node.clone(), false));
        }
        let mut opened = None;
        for (node, _picked) in &order {
            let dir = shard_copy_dir(data_dir, node, shard);
            match open_one(&dir) {
                Ok((readers, tombstones)) => {
                    router.note_success(shard, node);
                    opened = Some(OpenShard {
                        shard,
                        dir,
                        readers,
                        tombstones,
                    });
                    break;
                }
                Err(e) => {
                    router.note_failure(shard, node);
                    diagnostics.push(format!(
                        "shard {shard:02} copy {node} failed ({e}); failing over"
                    ));
                }
            }
        }
        let attempts = order.len();
        match opened {
            Some(s) => shards.push(s),
            None => {
                if attempts == 0 {
                    diagnostics.push(format!(
                        "shard {shard:02}: no healthy copy (partial results)"
                    ));
                } else {
                    diagnostics.push(format!(
                        "shard {shard:02}: all copies failed (partial results)"
                    ));
                }
            }
        }
    }
    (shards, diagnostics)
}

fn open_one(dir: &Path) -> Result<(Vec<SegmentReader>, Vec<u64>), String> {
    // A missing shard directory is a failure (failover/partial), not an
    // empty index: `load_or_new` would mask it. Fresh shards always have a
    // seeded manifest from `cluster init`.
    if !dir.exists() {
        return Err(format!("shard directory {} missing", dir.display()));
    }
    let manifest = Manifest::load_or_new(dir).map_err(|e| e.to_string())?;
    let mut readers = Vec::new();
    for name in &manifest.segments {
        readers.push(SegmentReader::open(&dir.join(name)).map_err(|e| format!("{name}: {e}"))?);
    }
    Ok((readers, manifest.deleted))
}

/// Gather global statistics across opened shards.
fn gather_global(
    shards: &[OpenShard],
    analysis: &AnalysisConfig,
    scoring_terms: &[(Field, String)],
) -> GlobalStats {
    let mut global = GlobalStats::default();
    for s in shards {
        let view = MultiSegmentView::new(&s.readers, &s.tombstones);
        let mut totals = [0u64; 4];
        for f in Field::ALL {
            totals[f.index()] = view.field_token_total(f, analysis);
        }
        let mut df = BTreeMap::new();
        for (f, t) in scoring_terms {
            let d = view.postings(*f, t).map(|p| p.len()).unwrap_or(0);
            *df.entry((*f, t.clone())).or_insert(0) += d;
        }
        let mut max_timestamp = 0u64;
        for id in view.all_doc_ids() {
            if let Some(d) = view.get(id) {
                max_timestamp = max_timestamp.max(d.timestamp);
            }
        }
        global.add_shard(view.len(), totals, df, max_timestamp);
    }
    global
}

/// Union vocabulary across shards for comparable prefix/fuzzy expansion.
fn union_vocab(shards: &[OpenShard]) -> BTreeMap<Field, Vec<String>> {
    let mut vocab: BTreeMap<Field, Vec<String>> = BTreeMap::new();
    for f in Field::ALL {
        let mut set = std::collections::BTreeSet::new();
        for s in shards {
            let view = MultiSegmentView::new(&s.readers, &s.tombstones);
            for t in view.field_terms(f) {
                set.insert(t);
            }
        }
        vocab.insert(f, set.into_iter().collect());
    }
    vocab
}

/// Distributed ranked search: global stats → parallel local top-N under
/// global stats → global merge. Exact vs single-index (§63).
pub async fn distributed_search(
    data_dir: &Path,
    topo: &Topology,
    router: &mut Router,
    analysis: &AnalysisConfig,
    ranking: &RankingConfig,
    query: &Query,
    top_k: usize,
) -> CoordinatorResult {
    let start = Instant::now();
    let (shards, mut diagnostics) = open_shards(data_dir, topo, router);
    // Partial iff any shard is unserved — successful failovers (a copy
    // failed but the shard still served) are NOT partial.
    let partial = shards.len() < topo.shard_count;
    if shards.is_empty() {
        diagnostics.push("no shards available".into());
        return CoordinatorResult {
            hits: Vec::new(),
            docs: HashMap::new(),
            partial: true,
            diagnostics,
            per_shard_ms: Vec::new(),
            took_ms: start.elapsed().as_secs_f64() * 1000.0,
        };
    }

    // Phase 1: global vocabulary + scoring terms + statistics.
    let vocab = union_vocab(&shards);
    let scoring_terms = expand_terms_from_vocab(&vocab, query);
    let global = gather_global(&shards, analysis, &scoring_terms);
    let collection = global.as_collection_stats(&scoring_terms);

    // Phase 2: parallel local top-N under global stats (spawn_blocking:
    // readers are in-RAM, scoring is CPU-bound).
    let mut tasks = Vec::new();
    for s in shards {
        let (readers, tombstones) = (s.readers, s.tombstones);
        let (shard_id, query_c, collection_c) = (s.shard, query.clone(), collection.clone());
        let (analysis_c, ranking_c) = (analysis.clone(), ranking.clone());
        tasks.push((
            shard_id,
            tokio::task::spawn_blocking(move || {
                let t0 = Instant::now();
                let view = MultiSegmentView::new(&readers, &tombstones);
                let candidates =
                    sealion_query::execute::search_view(&view, &query_c).unwrap_or_default();
                let (hits, _) = ranked_search_with_stats(
                    &view,
                    &analysis_c,
                    &ranking_c,
                    &query_c,
                    top_k,
                    &candidates,
                    Some(collection_c.clone()),
                )
                .unwrap_or_else(|_| (Vec::new(), collection_c));
                // Keep owning docs for the merge display.
                let docs: HashMap<DocId, Document> = view
                    .all_doc_ids()
                    .into_iter()
                    .filter_map(|id| view.get(id).map(|d| (id, d.clone())))
                    .collect();
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                (hits, docs, ms)
            }),
        ));
    }
    // Phase 3: global merge (scores comparable → plain sort + truncate).
    let mut merged: Vec<RankedHit> = Vec::new();
    let mut docs: HashMap<DocId, Document> = HashMap::new();
    let mut per_shard_ms = Vec::new();
    for (shard_id, task) in tasks {
        match task.await {
            Ok((hits, shard_docs, ms)) => {
                per_shard_ms.push((shard_id, ms));
                merged.extend(hits);
                docs.extend(shard_docs);
            }
            Err(e) => {
                diagnostics.push(format!(
                    "shard {shard_id:02} task failed: {e} (partial results)"
                ));
            }
        }
    }
    let partial = partial || per_shard_ms.len() < topo.shard_count;
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc.cmp(&b.doc))
    });
    merged.truncate(top_k);
    // Drop merged hits whose docs vanished (shouldn't happen; defensive).
    merged.retain(|h| docs.contains_key(&h.doc));
    CoordinatorResult {
        hits: merged,
        docs,
        partial,
        diagnostics,
        per_shard_ms,
        took_ms: start.elapsed().as_secs_f64() * 1000.0,
    }
}
