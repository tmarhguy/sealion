//! `sealion` command-line interface (spec §103).
//!
//! Subcommands are wired end-to-end as milestones land. Until then each
//! unimplemented subcommand prints a clear "not yet implemented" message
//! with the milestone that will deliver it, rather than failing obscurely.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

mod local;

#[derive(Parser)]
#[command(
    name = "sealion",
    version,
    about = "SeaLion distributed full-text search engine"
)]
struct Cli {
    /// Path to TOML config file (spec §104).
    #[arg(long, global = true)]
    config: Option<String>,
    /// Index data directory (default `./sealion-data`).
    #[arg(long, global = true)]
    data_dir: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a local corpus directory (spec §6).
    Index(IndexArgs),
    /// Crawl the web from seed URLs (spec §6).
    Crawl(CrawlArgs),
    /// Search the index (spec §76).
    Search(SearchArgs),
    /// Prefix completions from the live vocabulary (spec §46).
    Complete(CompleteArgs),
    /// Cluster operations (spec §60–71).
    Cluster(ClusterArgs),
    /// Shard operations.
    Shard(ShardArgs),
    /// Node operations.
    Node(NodeArgs),
    /// Relevance evaluation (spec §51–53).
    Eval(EvalArgs),
    /// Benchmarks (spec §85–93).
    Bench(BenchArgs),
    /// Serve the HTTP search + admin API (spec §76–77).
    Serve(ServeArgs),
}

#[derive(Args)]
struct IndexArgs {
    #[command(subcommand)]
    op: Option<IndexOp>,
    /// Corpus path for `sealion index <path>`.
    path: Option<String>,
}

#[derive(Subcommand)]
enum IndexOp {
    Stats,
    Verify,
    Merge,
    /// Tombstone a document by numeric DocId without rebuilding.
    Delete {
        /// Numeric document ID (see search output `[id]`).
        id: u64,
    },
    /// Compute PageRank authority over the link graph and publish it
    /// as a new generation (spec §54).
    Authority {
        /// PageRank damping factor.
        #[arg(long, default_value_t = 0.85)]
        damping: f64,
        /// PageRank iterations.
        #[arg(long, default_value_t = 50)]
        iterations: usize,
    },
}

#[derive(Args)]
struct CrawlArgs {
    /// Seed URLs.
    seeds: Vec<String>,
    /// Maximum link-following depth from seeds.
    #[arg(long)]
    depth: Option<usize>,
    /// Maximum pages accepted from this run.
    #[arg(long)]
    pages: Option<usize>,
}

#[derive(Args)]
struct SearchArgs {
    /// Query string.
    query: String,
    /// Explain ranking (§50).
    #[arg(long)]
    explain: bool,
    /// Use exhaustive scoring instead of Block-Max WAND (§38–39).
    /// Both are exact (§41); this only changes the work performed.
    #[arg(long)]
    exhaustive: bool,
}

#[derive(Args)]
struct CompleteArgs {
    /// Prefix to complete (normalized through the index pipeline).
    prefix: String,
    /// Field scope (`title`/`heading`/`body`/`anchor`; default all fields).
    #[arg(long)]
    field: Option<String>,
    /// Maximum completions.
    #[arg(long, default_value_t = 8)]
    limit: usize,
}

#[derive(Args)]
struct ClusterArgs {
    #[command(subcommand)]
    op: ClusterOp,
}

#[derive(Subcommand)]
enum ClusterOp {
    /// Show shards, copies, health, and generations.
    Status,
    /// Create a cluster layout: N shards across R nodes (directories).
    Init {
        /// Number of shards.
        #[arg(long, default_value_t = 4)]
        shards: usize,
        /// Replica nodes (each shard copied to this many nodes).
        #[arg(long, default_value_t = 1)]
        replicas: usize,
    },
    /// Move one shard copy to another node (online rebalance, §71).
    Move {
        /// Shard index.
        shard: usize,
        /// Destination node name.
        node: String,
        /// Source node (default: first listed copy).
        #[arg(long)]
        from: Option<String>,
    },
}

#[derive(Args)]
struct ShardArgs {
    #[command(subcommand)]
    op: ShardOp,
}

#[derive(Subcommand)]
enum ShardOp {
    List,
}

#[derive(Args)]
struct NodeArgs {
    #[command(subcommand)]
    op: NodeOp,
}

#[derive(Subcommand)]
enum NodeOp {
    List,
}

#[derive(Args)]
struct EvalArgs {
    #[command(subcommand)]
    op: EvalOp,
}

#[derive(Subcommand)]
enum EvalOp {
    /// Run the relevance harness (nDCG@10, MRR, MAP) over evaluation/.
    Relevance {
        /// Evaluation directory (default `./evaluation`).
        #[arg(long)]
        eval_dir: Option<String>,
        /// Save this report as JSON for later `--baseline` comparison.
        #[arg(long)]
        save: Option<String>,
        /// Compare against a saved report (prints deltas + regressions).
        #[arg(long)]
        baseline: Option<String>,
    },
}

#[derive(Args)]
struct BenchArgs {
    #[command(subcommand)]
    op: BenchOp,
}

#[derive(Args)]
struct ServeArgs {
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,
    /// Bearer token for /api/admin/* (unset = open, development only).
    #[arg(long)]
    admin_token: Option<String>,
}

#[derive(Subcommand)]
enum BenchOp {
    /// Query throughput/latency over the query mix (§88).
    Query {
        /// Benchmark rounds over the mix.
        #[arg(long, default_value_t = 3)]
        rounds: usize,
        /// Enable the query-result cache (reports hit rate, §72).
        #[arg(long)]
        cache: bool,
        /// Score exhaustively (ablation baseline vs Block-Max WAND, §90).
        #[arg(long)]
        exhaustive: bool,
    },
    /// Indexing throughput over a corpus directory (§87).
    Index {
        /// Corpus path to index into a temp segment.
        path: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let cfg = match &cli.config {
        Some(path) => {
            let toml = std::fs::read_to_string(path)?;
            let cfg = sealion_core::config::Config::from_toml(&toml)?;
            tracing::info!(?path, "using config file");
            cfg
        }
        None => sealion_core::config::Config::default(),
    };
    let data_dir =
        std::path::PathBuf::from(cli.data_dir.unwrap_or_else(|| "./sealion-data".into()));
    match cli.command {
        Command::Index(a) => match a.op {
            None => match a.path {
                Some(path) => {
                    if local::is_cluster(&data_dir) {
                        local::index_corpus_clustered(
                            std::path::Path::new(&path),
                            &data_dir,
                            &cfg.analysis,
                        )
                    } else {
                        local::index_corpus(std::path::Path::new(&path), &data_dir, &cfg.analysis)
                    }
                }
                None => {
                    println!("usage: sealion index <corpus-path> | sealion index stats|verify");
                    Ok(())
                }
            },
            Some(IndexOp::Stats) => local::print_stats(&data_dir),
            Some(IndexOp::Verify) => local::verify(&data_dir),
            Some(IndexOp::Merge) => {
                if local::is_cluster(&data_dir) {
                    local::merge_cluster(&data_dir, &cfg.analysis)
                } else {
                    local::merge_segments(&data_dir, &cfg.analysis)
                }
            }
            Some(IndexOp::Delete { id }) => {
                if local::is_cluster(&data_dir) {
                    local::delete_clustered(&data_dir, id)
                } else {
                    local::delete_document(&data_dir, id)
                }
            }
            Some(IndexOp::Authority {
                damping,
                iterations,
            }) => local::index_authority(&data_dir, &cfg.analysis, damping, iterations),
        },
        Command::Crawl(a) => {
            if a.seeds.is_empty() {
                println!("usage: sealion crawl <seed-url>... [--depth N] [--pages N]");
                return Ok(());
            }
            let mut crawl_cfg = cfg.crawler.clone();
            if let Some(d) = a.depth {
                crawl_cfg.max_depth = d;
            }
            if let Some(p) = a.pages {
                crawl_cfg.max_pages = p;
            }
            // Validate the overridden crawler knobs via a full config check.
            let check_cfg = sealion_core::config::Config {
                crawler: crawl_cfg.clone(),
                ..cfg.clone()
            };
            if let Err(e) = check_cfg.validate() {
                return Err(anyhow::anyhow!("invalid crawler config: {e}"));
            }
            // Frontier state lives next to the index for resumability.
            let state_dir = data_dir.join("crawl-state");
            local::crawl_seeds(&a.seeds, &data_dir, &cfg.analysis, &crawl_cfg, &state_dir).await
        }
        Command::Search(a) => {
            // Cluster mode fans out through the coordinator (§62–69).
            if local::is_cluster(&data_dir) {
                return local::cluster_search(&data_dir, &cfg, &a.query, a.explain).await;
            }
            // Ranked BM25 search over persistent segments (milestone 07).
            // Full query language via the milestone 08 parser; Block-Max
            // WAND executes it exactly (milestone 09).
            use sealion_index::view::{IndexView as _, MultiSegmentView};
            let gen = local::load_generation(&data_dir)?;
            let view = MultiSegmentView::new(&gen.readers, &gen.tombstones);
            let query =
                sealion_query::parser::parse(&cfg.analysis, cfg.query.max_query_terms, &a.query)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            let start = std::time::Instant::now();
            // Block-Max WAND is exact (§41): same hits as exhaustive, less
            // scoring work. `--exhaustive` selects the baseline instead.
            let (hits, work) = if a.exhaustive {
                let hits = sealion_query::rank::ranked_search(
                    &view,
                    &cfg.analysis,
                    &cfg.ranking,
                    &query,
                    cfg.query.top_k,
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
                (hits, None)
            } else {
                let (hits, stats) = sealion_query::wand::block_max_wand_search(
                    &view,
                    &cfg.analysis,
                    &cfg.ranking,
                    &query,
                    cfg.query.top_k,
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
                (hits, Some(stats))
            };
            let took_ms = start.elapsed().as_secs_f64() * 1000.0;
            if let Some(s) = &work {
                tracing::info!(
                    scored = s.scored,
                    skipped = s.skipped,
                    blocks_skipped = s.blocks_skipped,
                    "block-max-wand work"
                );
            }
            let by_id = local::resolve_docs(&gen.readers, &gen.tombstones);
            // Expanded terms so prefix/fuzzy hits highlight their expansions.
            let expanded = sealion_query::rank::expand_terms(&view, &query);
            let terms: Vec<(Option<sealion_core::field::Field>, &str)> = expanded
                .iter()
                .map(|(f, t)| (Some(*f), t.as_str()))
                .collect();
            if a.explain {
                match &work {
                    Some(s) => println!(
                        "{} result(s) in {took_ms:.2}ms (partial: false, block-max-wand: scored={} skipped={} blocks_skipped={})",
                        hits.len(),
                        s.scored,
                        s.skipped,
                        s.blocks_skipped
                    ),
                    None => println!(
                        "{} result(s) in {took_ms:.2}ms (partial: false, exhaustive)",
                        hits.len()
                    ),
                }
                for h in &hits {
                    let title = by_id
                        .get(&h.doc)
                        .map(|d| d.title.as_str())
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
            println!(
                "{} result(s) in {took_ms:.2}ms (partial: false, BM25 ranked)",
                hits.len()
            );
            if hits.is_empty() {
                // Did-you-mean (§43): best correction per query term. Terms
                // from the parser are already normalized, so they probe
                // the vocabulary directly (re-analyzing would stem twice).
                let mut fixes: Vec<String> = Vec::new();
                let mut seen_probe = std::collections::BTreeSet::new();
                let mut seen_fix = std::collections::BTreeSet::new();
                for (field, term) in query.terms() {
                    // Same term may appear under several fields; try each
                    // field once, but show each suggested term only once.
                    if !seen_probe.insert((field, term.to_string())) {
                        continue;
                    }
                    // Skip terms that already match (e.g. NOT arms).
                    if view
                        .postings(field.unwrap_or(sealion_core::field::Field::Body), term)
                        .map(|p| !p.is_empty())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let s = sealion_query::spell::suggest(&view, field, term, 2, 1);
                    if let Some(best) = s.into_iter().next() {
                        if seen_fix.insert(best.term.clone()) {
                            fixes.push(format!("{}→{}", term, best.term));
                        }
                    }
                }
                if !fixes.is_empty() {
                    println!("did you mean: {}?", fixes.join(", "));
                }
            }
            for h in &hits {
                match by_id.get(&h.doc) {
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
        Command::Complete(a) => {
            use sealion_index::analysis::Analyzer;
            use sealion_index::view::MultiSegmentView;
            let gen = local::load_generation(&data_dir)?;
            let view = MultiSegmentView::new(&gen.readers, &gen.tombstones);
            let field = match &a.field {
                None => None,
                Some(name) => match sealion_query::spell::field_named(name) {
                    Some(f) => Some(f),
                    None => {
                        println!("unknown field `{name}` (title/heading/body/anchor)");
                        return Ok(());
                    }
                },
            };
            // Normalize the prefix through the pipeline (first token).
            let analyzer = Analyzer::new(&cfg.analysis);
            let scope = field.unwrap_or(sealion_core::field::Field::Body);
            let norm = analyzer.analyze(scope, &a.prefix);
            let Some(prefix) = norm.first().map(|t| t.term.clone()) else {
                println!("no completions (prefix analyzes to nothing)");
                return Ok(());
            };
            let start = std::time::Instant::now();
            let out = sealion_query::spell::complete(&view, field, &prefix, a.limit.min(100));
            let took_ms = start.elapsed().as_secs_f64() * 1000.0;
            if out.is_empty() {
                println!("no completions for `{}` ({took_ms:.2}ms)", a.prefix);
            } else {
                for c in &out {
                    println!("  {:?}:{} (df={})", c.field, c.term, c.df);
                }
                eprintln!("({} completion(s) in {took_ms:.2}ms)", out.len());
            }
            Ok(())
        }
        Command::Cluster(a) => match a.op {
            ClusterOp::Status => local::cluster_status(&data_dir),
            ClusterOp::Init { shards, replicas } => {
                local::cluster_init(&data_dir, shards, replicas)
            }
            ClusterOp::Move { shard, node, from } => {
                local::cluster_move(&data_dir, shard, &node, from.as_deref())
            }
        },
        Command::Shard(_) => local::shard_list(&data_dir),
        Command::Node(_) => local::node_list(&data_dir),
        Command::Eval(a) => match a.op {
            EvalOp::Relevance {
                eval_dir,
                save,
                baseline,
            } => local::eval_relevance(
                std::path::Path::new(&eval_dir.unwrap_or_else(|| "./evaluation".into())),
                &cfg,
                save.as_deref(),
                baseline.as_deref(),
            ),
        },
        Command::Serve(a) => {
            let cfg = sealion_api::ServerConfig {
                data_dir: data_dir.clone(),
                config: cfg.clone(),
                admin_token: a.admin_token.clone(),
            };
            if let Err(e) = sealion_api::serve(cfg, &a.bind).await {
                return Err(anyhow::anyhow!("{e}"));
            }
            Ok(())
        }
        Command::Bench(b) => match b.op {
            BenchOp::Query {
                rounds,
                cache,
                exhaustive,
            } => local::bench_query(&data_dir, &cfg, rounds, cache, exhaustive),
            BenchOp::Index { path } => {
                local::bench_index(std::path::Path::new(&path), &cfg.analysis)
            }
        },
    }
}
