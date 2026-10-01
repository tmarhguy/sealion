//! SeaLion benchmark harnesses: indexing, query, compression, WAND ablation,
//! distributed scaling, failure, relevance (spec §85–93).
//!
//! Milestone 16 ships the query and indexing harnesses (`bench query`,
//! `bench index`) plus the WAND ablation (§90). Relevance (§93) reuses
//! `sealion-eval`; distributed/failure suites land with their milestones.

use sealion_core::config::{AnalysisConfig, RankingConfig};
use sealion_index::view::IndexView;
use serde::{Deserialize, Serialize};

/// Latency summary over timed runs (milliseconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Latency {
    pub runs: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl Latency {
    pub fn from_ms(mut samples: Vec<f64>) -> Self {
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let runs = samples.len();
        let mean_ms = if runs == 0 {
            0.0
        } else {
            samples.iter().sum::<f64>() / runs as f64
        };
        let pct = |p: f64| {
            if samples.is_empty() {
                0.0
            } else {
                samples[((p * runs as f64).ceil() as usize)
                    .saturating_sub(1)
                    .min(runs - 1)]
            }
        };
        Self {
            runs,
            mean_ms,
            p50_ms: pct(0.50),
            p95_ms: pct(0.95),
            p99_ms: pct(0.99),
        }
    }
}

/// Standard query mix (§86): rare/common terms, AND widths, OR, phrase,
/// prefix, fuzzy. Callers filter to what their corpus supports.
pub fn query_mix() -> Vec<&'static str> {
    vec![
        "zephyrquux",                                  // rare term (likely empty)
        "compiler",                                    // common term
        "compiler optimization",                       // 2-term AND
        "distributed database systems consensus raft", // 5-term AND
        "compiler OR database",                        // OR
        "\"database systems\"",                        // phrase
        "comp*",                                       // prefix
        "compiler~1",                                  // fuzzy
        "title:compiler",                              // field filter
    ]
}

/// Query benchmark report (§88).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryBench {
    pub queries: usize,
    pub rounds: usize,
    pub latency: Latency,
    pub qps: f64,
    pub cache_hit_rate: f64,
    pub wand_scored_avg: f64,
    pub wand_skipped_avg: f64,
}

/// Battery inputs: one struct instead of a 10-argument call.
pub struct QueryBenchInput<'a, V: IndexView> {
    pub view: &'a V,
    pub analysis: &'a AnalysisConfig,
    pub ranking: &'a RankingConfig,
    pub queries: &'a [String],
    pub top_k: usize,
    pub max_terms: usize,
    pub generation: u64,
    pub rounds: usize,
    pub cache: Option<&'a mut sealion_query::cache::QueryCache>,
    pub exhaustive: bool,
}

/// Run the query battery against any index view. When `cache` is passed,
/// identical (query, generation) repeats hit it and the hit rate is
/// reported — the §72 justification workload. `exhaustive` selects the
/// baseline path for the §90 ablation.
pub fn run_query_bench<V: IndexView>(input: QueryBenchInput<'_, V>) -> QueryBench {
    let QueryBenchInput {
        view,
        analysis,
        ranking,
        queries,
        top_k,
        max_terms,
        generation,
        rounds,
        cache,
        exhaustive,
    } = input;
    let mut samples = Vec::new();
    let mut scored_sum = 0u64;
    let mut skipped_sum = 0u64;
    let mut timed_queries = 0usize;
    // Cache reuse needs the same cache across rounds: take it out of the
    // Option once (callers pass None for uncached baselines).
    let mut cache = cache;
    for _ in 0..rounds {
        for raw in queries {
            let t0 = std::time::Instant::now();
            let mut served = false;
            if let Some(c) = cache.as_mut() {
                if c.get(raw, top_k, ranking, generation).is_some() {
                    served = true;
                }
            }
            if !served {
                let query = sealion_query::parser::parse(analysis, max_terms, raw)
                    .unwrap_or(sealion_query::query::Query::MatchNothing);
                if exhaustive {
                    let hits =
                        sealion_query::rank::ranked_search(view, analysis, ranking, &query, top_k)
                            .unwrap_or_default();
                    // Exhaustive scores every Boolean candidate.
                    scored_sum += sealion_query::execute::search_view(view, &query)
                        .map(|v| v.len() as u64)
                        .unwrap_or(0);
                    if let Some(c) = cache.as_mut() {
                        c.put(raw, top_k, ranking, generation, hits);
                    }
                } else {
                    let (hits, stats) = sealion_query::wand::block_max_wand_search(
                        view, analysis, ranking, &query, top_k,
                    )
                    .unwrap_or((Vec::new(), Default::default()));
                    scored_sum += stats.scored as u64;
                    skipped_sum += stats.skipped as u64;
                    if let Some(c) = cache.as_mut() {
                        c.put(raw, top_k, ranking, generation, hits);
                    }
                }
            }
            samples.push(t0.elapsed().as_secs_f64() * 1000.0);
            timed_queries += 1;
        }
    }
    let total_s: f64 = samples.iter().sum::<f64>() / 1000.0;
    QueryBench {
        queries: timed_queries,
        rounds,
        latency: Latency::from_ms(samples),
        qps: timed_queries as f64 / total_s.max(1e-9),
        cache_hit_rate: cache.as_ref().map(|c| c.hit_rate()).unwrap_or(0.0),
        wand_scored_avg: scored_sum as f64 / timed_queries.max(1) as f64,
        wand_skipped_avg: skipped_sum as f64 / timed_queries.max(1) as f64,
    }
}

/// Indexing benchmark report (§87).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexBench {
    pub documents: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub terms: usize,
    pub secs: f64,
    pub docs_per_sec: f64,
    pub mb_per_sec: f64,
}
