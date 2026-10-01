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
}

#[derive(Args)]
struct CrawlArgs {
    /// Seed URLs.
    seeds: Vec<String>,
}

#[derive(Args)]
struct SearchArgs {
    /// Query string.
    query: String,
    /// Explain ranking (§50).
    #[arg(long)]
    explain: bool,
}

#[derive(Args)]
struct ClusterArgs {
    #[command(subcommand)]
    op: ClusterOp,
}

#[derive(Subcommand)]
enum ClusterOp {
    Status,
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
    Relevance,
}

#[derive(Args)]
struct BenchArgs {
    #[command(subcommand)]
    op: BenchOp,
}

#[derive(Subcommand)]
enum BenchOp {
    Query,
    Index,
}

/// A stub: prints which milestone delivers this subcommand.
fn stub(milestone: u8, what: &str) -> Result<()> {
    println!("sealion: `{what}` is not yet implemented (milestone {milestone:02}).");
    println!("See docs/architecture.md for the build plan.");
    Ok(())
}

fn main() -> Result<()> {
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
                    local::index_corpus(std::path::Path::new(&path), &data_dir, &cfg.analysis)
                }
                None => {
                    println!("usage: sealion index <corpus-path> | sealion index stats|verify");
                    Ok(())
                }
            },
            Some(IndexOp::Stats) => local::print_stats(&data_dir),
            Some(IndexOp::Verify) => local::verify(&data_dir),
            Some(IndexOp::Merge) => stub(6, "index merge"),
        },
        Command::Crawl(_) => stub(11, "crawl"),
        Command::Search(a) => {
            if a.explain {
                return stub(7, &format!("search \"{}\" --explain", a.query));
            }
            // Boolean search over persistent segments. Ranked BM25 search
            // arrives in milestone 07; results are DocId-sorted.
            let readers = local::open_segments(&data_dir)?;
            let start = std::time::Instant::now();
            let hits = local::search_all(&readers, &cfg.analysis, &a.query)?;
            let took_ms = start.elapsed().as_secs_f64() * 1000.0;
            // Resolve stored documents for display.
            let by_id: std::collections::HashMap<_, _> = readers
                .iter()
                .flat_map(|r| {
                    r.all_doc_ids()
                        .into_iter()
                        .filter_map(|id| r.get(id).map(|d| (id, d)))
                })
                .collect();
            println!(
                "{} result(s) in {took_ms:.2}ms (partial: false, unranked boolean)",
                hits.len()
            );
            for id in hits.iter().take(cfg.query.top_k) {
                match by_id.get(id) {
                    Some(d) => println!("  [{}] {} -- {}", id.0, d.title, d.url),
                    None => println!("  [{}] <missing>", id.0),
                }
            }
            Ok(())
        }
        Command::Cluster(_) => stub(13, "cluster status"),
        Command::Shard(_) => stub(13, "shard list"),
        Command::Node(_) => stub(14, "node list"),
        Command::Eval(_) => stub(12, "eval relevance"),
        Command::Bench(b) => match b.op {
            BenchOp::Query => stub(20, "bench query"),
            BenchOp::Index => stub(20, "bench index"),
        },
    }
}
