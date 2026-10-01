//! SeaLion HTTP API: search and admin endpoints (spec §76–77, §82).
//!
//! - `GET /api/search?q=...&limit=...[&explain=true]` — ranked search with
//!   snippets, `took_ms`, and explicit `partial` (§76, §69). Zero-hit
//!   simple queries may carry a `suggestion` correction (§43).
//! - `GET /api/complete?prefix=...&limit=...` — ranked completions (§46).
//! - `GET /api/admin/status` — index/cluster generations, counts (§77).
//! - `GET /api/admin/metrics` — counters in JSON (§83 subset).
//! - `GET /`, `/search`, `/stats`, `/about` — the single-page search
//!   frontend (static HTML/JS; the React product is future work — this
//!   proves the API contract it will consume).
//!
//! Long-lived process: the query-result cache (§72) finally has reuse
//! across requests. Admin endpoints take `Authorization: Bearer <token>`
//! when the server is started with one (§77); development default is open.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use sealion_core::config::Config;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    pub config: Config,
    pub admin_token: Option<String>,
}

#[derive(Debug, Default)]
struct Counters {
    searches: AtomicU64,
    cache_hits: AtomicU64,
    partials: AtomicU64,
    completes: AtomicU64,
}

#[derive(Debug, Clone)]
struct AppState {
    cfg: ServerConfig,
    cache: Arc<Mutex<sealion_query::cache::QueryCache>>,
    counters: Arc<Counters>,
}

#[derive(Debug, Serialize)]
struct SearchHit {
    doc_id: u64,
    title: String,
    url: String,
    score: f32,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<String>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    query: String,
    took_ms: f64,
    partial: bool,
    trace_ms: HashMap<String, f64>,
    results: Vec<SearchHit>,
    /// BK-tree correction for zero-hit simple queries ("did you mean").
    /// Absent when results exist or no confident correction was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    explain: bool,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, serde::Deserialize)]
struct CompleteParams {
    prefix: String,
    #[serde(default = "default_complete_limit")]
    limit: usize,
}

fn default_complete_limit() -> usize {
    8
}

fn check_admin(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    match &state.cfg.admin_token {
        None => Ok(()),
        Some(token) => {
            let got = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if got == format!("Bearer {token}") {
                Ok(())
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }
}

fn is_cluster(data_dir: &std::path::Path) -> bool {
    sealion_distributed::shard::Topology::path(data_dir).exists()
}

async fn search_handler(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let t0 = Instant::now();
    let mut trace = HashMap::new();
    state.counters.searches.fetch_add(1, Ordering::Relaxed);
    let limit = params.limit.clamp(1, 100);

    let t_parse = Instant::now();
    let query = match sealion_query::parser::parse(
        &state.cfg.config.analysis,
        state.cfg.config.query.max_query_terms,
        &params.q,
    ) {
        Ok(q) => q,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    trace.insert(
        "parse_ms".to_string(),
        t_parse.elapsed().as_secs_f64() * 1000.0,
    );

    // Cache lookup (generation-gated): hits skip search entirely.
    let generation = manifest_generation(&state.cfg.data_dir);
    let mut cached_hits = None;
    if let Ok(mut cache) = state.cache.lock() {
        cached_hits = cache.get(&params.q, limit, &state.cfg.config.ranking, generation);
        if cached_hits.is_some() {
            state.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
        }
    }

    let t_search = Instant::now();
    let (hits, docs, partial, shard_ms) = match cached_hits {
        Some(hits) => {
            // Re-resolve stored docs for display (cheap metadata reads).
            let docs = resolve_docs(&state.cfg.data_dir);
            (hits, docs, false, Vec::new())
        }
        None => {
            let (hits, docs, partial, shard_ms) = if is_cluster(&state.cfg.data_dir) {
                run_cluster(&state, &query, limit).await
            } else {
                run_single(&state, &query, limit)
            };
            if let Ok(mut cache) = state.cache.lock() {
                cache.put(
                    &params.q,
                    limit,
                    &state.cfg.config.ranking,
                    generation,
                    hits.clone(),
                );
            }
            (hits, docs, partial, shard_ms)
        }
    };
    trace.insert(
        "search_ms".to_string(),
        t_search.elapsed().as_secs_f64() * 1000.0,
    );
    for (i, ms) in shard_ms {
        trace.insert(format!("shard_{i:02}_ms"), ms);
    }
    if partial {
        state.counters.partials.fetch_add(1, Ordering::Relaxed);
    }

    let t_snip = Instant::now();
    let terms = query.terms();
    let results: Vec<SearchHit> = hits
        .into_iter()
        .filter_map(|h| {
            docs.get(&h.doc).map(|d| {
                let snippet =
                    sealion_query::rank::make_snippet(d, &terms, &state.cfg.config.analysis, 200);
                SearchHit {
                    doc_id: h.doc.0,
                    title: d.title.clone(),
                    url: d.url.clone(),
                    score: h.score,
                    snippet,
                    explain: params.explain.then(|| explain_text(&h)),
                }
            })
        })
        .collect();
    trace.insert(
        "snippets_ms".to_string(),
        t_snip.elapsed().as_secs_f64() * 1000.0,
    );

    let took_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let suggestion = if results.is_empty() {
        suggest_correction(&state, &params.q)
    } else {
        None
    };
    Json(SearchResponse {
        query: params.q,
        took_ms,
        partial,
        trace_ms: trace,
        results,
        suggestion,
    })
    .into_response()
}

fn explain_text(h: &sealion_query::rank::RankedHit) -> String {
    let mut out = format!("total={:.4}", h.explain.total);
    for t in &h.explain.terms {
        out.push_str(&format!(" {:?}:{}={:.3}", t.field, t.term, t.contrib));
    }
    if h.explain.authority != 0.0 || h.explain.freshness != 0.0 {
        out.push_str(&format!(
            " authority={:.3} freshness={:.3}",
            h.explain.authority, h.explain.freshness
        ));
    }
    out
}

fn manifest_generation(data_dir: &std::path::Path) -> u64 {
    if is_cluster(data_dir) {
        sealion_distributed::shard::Topology::load(data_dir)
            .map(|t| t.shard_count as u64)
            .unwrap_or(0)
    } else {
        sealion_index::segment::manifest::Manifest::load_or_new(data_dir)
            .map(|m| m.generation)
            .unwrap_or(0)
    }
}

type SearchTriple = (
    Vec<sealion_query::rank::RankedHit>,
    HashMap<sealion_core::document::DocId, sealion_core::document::Document>,
    bool,
    Vec<(usize, f64)>,
);

/// Stored docs for display (newest-wins; used by cache-hit snippet path).
fn resolve_docs(
    data_dir: &std::path::Path,
) -> HashMap<sealion_core::document::DocId, sealion_core::document::Document> {
    use sealion_index::segment::manifest::Manifest;
    use sealion_index::segment::reader::SegmentReader;
    use sealion_index::view::{IndexView, MultiSegmentView};
    let Ok(manifest) = Manifest::load_or_new(data_dir) else {
        return HashMap::new();
    };
    let readers: Vec<SegmentReader> = manifest
        .segments
        .iter()
        .filter_map(|n| SegmentReader::open(&data_dir.join(n)).ok())
        .collect();
    let view = MultiSegmentView::new(&readers, &manifest.deleted);
    view.all_doc_ids()
        .into_iter()
        .filter_map(|id| view.get(id).map(|d| (id, d.clone())))
        .collect()
}

fn run_single(state: &AppState, query: &sealion_query::query::Query, limit: usize) -> SearchTriple {
    use sealion_index::segment::manifest::Manifest;
    use sealion_index::segment::reader::SegmentReader;
    use sealion_index::view::{IndexView, MultiSegmentView};
    let data_dir = &state.cfg.data_dir;
    let manifest = match Manifest::load_or_new(data_dir) {
        Ok(m) => m,
        Err(_) => return (Vec::new(), HashMap::new(), true, Vec::new()),
    };
    let mut readers = Vec::new();
    for name in &manifest.segments {
        match SegmentReader::open(&data_dir.join(name)) {
            Ok(r) => readers.push(r),
            Err(_) => return (Vec::new(), HashMap::new(), true, Vec::new()),
        }
    }
    let view = MultiSegmentView::new(&readers, &manifest.deleted);
    let (hits, _) = sealion_query::wand::block_max_wand_search(
        &view,
        &state.cfg.config.analysis,
        &state.cfg.config.ranking,
        query,
        limit,
    )
    .unwrap_or((Vec::new(), Default::default()));
    let docs = view
        .all_doc_ids()
        .into_iter()
        .filter_map(|id| view.get(id).map(|d| (id, d.clone())))
        .collect();
    (hits, docs, false, Vec::new())
}

async fn run_cluster(
    state: &AppState,
    query: &sealion_query::query::Query,
    limit: usize,
) -> SearchTriple {
    use sealion_distributed::replica::Router;
    let topo = match sealion_distributed::shard::Topology::load(&state.cfg.data_dir) {
        Ok(t) => t,
        Err(_) => return (Vec::new(), HashMap::new(), true, Vec::new()),
    };
    let mut router = Router::default();
    let res = sealion_distributed::distributed_search(
        &state.cfg.data_dir,
        &topo,
        &mut router,
        &state.cfg.config.analysis,
        &state.cfg.config.ranking,
        query,
        limit,
    )
    .await;
    (res.hits, res.docs, res.partial, res.per_shard_ms)
}

/// Open servable segment readers for vocabulary-level work (completion,
/// spell suggestion). Single mode reads the data directory; cluster mode
/// reads the first servable shard's vocabulary (representative sample;
/// global-vocab work is documented future work). Returns readers plus
/// the tombstone list to hide.
fn open_readers(
    data_dir: &std::path::Path,
) -> (Vec<sealion_index::segment::reader::SegmentReader>, Vec<u64>) {
    use sealion_index::segment::manifest::Manifest;
    use sealion_index::segment::reader::SegmentReader;
    if is_cluster(data_dir) {
        match sealion_distributed::shard::Topology::load(data_dir) {
            Ok(topo) => {
                let mut all = Vec::new();
                let servable = (0..topo.shard_count).find_map(|s| {
                    topo.healthy_copies(s)
                        .into_iter()
                        .next()
                        .map(|c| sealion_distributed::shard::shard_copy_dir(data_dir, &c.node, s))
                });
                if let Some(dir) = servable {
                    if let Ok(m) = Manifest::load_or_new(&dir) {
                        for name in &m.segments {
                            if let Ok(r) = SegmentReader::open(&dir.join(name)) {
                                all.push(r);
                            }
                        }
                    }
                }
                (all, Vec::new())
            }
            Err(_) => (Vec::new(), Vec::new()),
        }
    } else {
        match Manifest::load_or_new(data_dir) {
            Ok(m) => {
                let readers: Vec<SegmentReader> = m
                    .segments
                    .iter()
                    .filter_map(|n| SegmentReader::open(&data_dir.join(n)).ok())
                    .collect();
                (readers, m.deleted)
            }
            Err(_) => (Vec::new(), Vec::new()),
        }
    }
}

/// BK-tree "did you mean" for zero-hit simple term queries. Returns a
/// corrected raw query string, or None when the query uses operators
/// (phrases, fields, parens), every term is already in-vocabulary, or no
/// confident (distance <= 2) correction exists. Never rewrites silently:
/// callers present this as a suggestion the user can accept.
fn suggest_correction(state: &AppState, raw: &str) -> Option<String> {
    use sealion_index::analysis::Analyzer;
    use sealion_index::view::{IndexView as _, MultiSegmentView};
    // Operators carry their own meaning; only plain term lists qualify.
    if raw.chars().any(|c| "\"():".contains(c)) {
        return None;
    }
    let (readers, tombstones) = open_readers(&state.cfg.data_dir);
    if readers.is_empty() {
        return None;
    }
    let view = MultiSegmentView::new(&readers, &tombstones);
    let analyzer = Analyzer::new(&state.cfg.config.analysis);
    let mut fixes: Vec<(String, String)> = Vec::new();
    for token in raw.split_whitespace() {
        let upper = token.to_ascii_uppercase();
        if matches!(upper.as_str(), "AND" | "OR" | "NOT") {
            continue;
        }
        for t in analyzer.analyze(sealion_core::field::Field::Body, token) {
            let known = sealion_core::field::Field::ALL.iter().any(|f| {
                view.postings(*f, &t.term)
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
            });
            if known {
                continue;
            }
            if let Some(s) = sealion_query::spell::suggest(&view, None, &t.term, 2, 1)
                .into_iter()
                .next()
            {
                fixes.push((t.term.clone(), s.term));
            }
        }
    }
    if fixes.is_empty() {
        return None;
    }
    let out: Vec<String> = raw
        .split_whitespace()
        .map(|token| {
            let terms = analyzer.analyze(sealion_core::field::Field::Body, token);
            for t in &terms {
                if let Some((_, fix)) = fixes.iter().find(|(bad, _)| bad == &t.term) {
                    return fix.clone();
                }
            }
            token.to_string()
        })
        .collect();
    let corrected = out.join(" ");
    (corrected != raw).then_some(corrected)
}

async fn complete_handler(
    State(state): State<AppState>,
    Query(params): Query<CompleteParams>,
) -> impl IntoResponse {
    state.counters.completes.fetch_add(1, Ordering::Relaxed);
    use sealion_index::analysis::Analyzer;
    use sealion_index::view::MultiSegmentView;
    let (readers, tombstones) = open_readers(&state.cfg.data_dir);
    let view = MultiSegmentView::new(&readers, &tombstones);
    let analyzer = Analyzer::new(&state.cfg.config.analysis);
    let norm = analyzer.analyze(sealion_core::field::Field::Body, &params.prefix);
    let Some(prefix) = norm.first().map(|t| t.term.clone()) else {
        return Json(serde_json::json!({ "prefix": params.prefix, "completions": [] }))
            .into_response();
    };
    let out = sealion_query::spell::complete(&view, None, &prefix, params.limit.clamp(1, 50));
    Json(serde_json::json!({
        "prefix": params.prefix,
        "completions": out.iter().map(|c| serde_json::json!({
            "field": format!("{:?}", c.field).to_lowercase(),
            "term": c.term,
            "df": c.df,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn status_handler(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(s) = check_admin(&state, &headers) {
        return s.into_response();
    }
    let data_dir = &state.cfg.data_dir;
    let (mode, detail) = if is_cluster(data_dir) {
        match sealion_distributed::shard::Topology::load(data_dir) {
            Ok(t) => (
                "cluster",
                serde_json::json!({
                    "shards": t.shard_count,
                    "nodes": t.nodes,
                    "copies": t.copies.len(),
                    "unavailable": t.unavailable_shards(),
                }),
            ),
            Err(e) => ("cluster-broken", serde_json::json!({ "error": e })),
        }
    } else {
        match sealion_index::segment::manifest::Manifest::load_or_new(data_dir) {
            Ok(m) => (
                "single",
                serde_json::json!({ "generation": m.generation, "segments": m.segments.len(), "tombstones": m.deleted.len() }),
            ),
            Err(e) => (
                "single-broken",
                serde_json::json!({ "error": e.to_string() }),
            ),
        }
    };
    Json(serde_json::json!({ "mode": mode, "detail": detail })).into_response()
}

async fn metrics_handler(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(s) = check_admin(&state, &headers) {
        return s.into_response();
    }
    let (hits, misses, evictions, cached) = match state.cache.lock() {
        Ok(c) => (c.hits, c.misses, c.evictions, c.len()),
        Err(_) => (0, 0, 0, 0),
    };
    Json(serde_json::json!({
        "searches": state.counters.searches.load(Ordering::Relaxed),
        "cache_hits": state.counters.cache_hits.load(Ordering::Relaxed),
        "partials": state.counters.partials.load(Ordering::Relaxed),
        "completes": state.counters.completes.load(Ordering::Relaxed),
        "cache": { "hits": hits, "misses": misses, "evictions": evictions, "entries": cached },
    }))
    .into_response()
}

const INDEX_HTML: &str = include_str!("index.html");

async fn root_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// Build the router (also used by tests via `serve`).
pub fn router(cfg: ServerConfig) -> axum::Router {
    let state = AppState {
        cfg,
        cache: Arc::new(Mutex::new(sealion_query::cache::QueryCache::new(256))),
        counters: Arc::new(Counters::default()),
    };
    axum::Router::new()
        .route("/", get(root_handler))
        // Client-side views of the single-page frontend; serving the same
        // bundle keeps shared/deep links working without server state.
        .route("/search", get(root_handler))
        .route("/stats", get(root_handler))
        .route("/about", get(root_handler))
        .route("/api/search", get(search_handler))
        .route("/api/complete", get(complete_handler))
        .route("/api/admin/status", get(status_handler))
        .route("/api/admin/metrics", get(metrics_handler))
        .with_state(state)
}

/// Serve on `addr` (e.g. `127.0.0.1:8080`) until cancelled.
pub async fn serve(cfg: ServerConfig, addr: &str) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    tracing::info!(%addr, "sealion api serving");
    serve_on(listener, cfg).await
}

/// Serve on a pre-bound listener (lets tests pick an ephemeral port).
pub async fn serve_on(listener: tokio::net::TcpListener, cfg: ServerConfig) -> Result<(), String> {
    axum::serve(listener, router(cfg))
        .await
        .map_err(|e| e.to_string())
}
