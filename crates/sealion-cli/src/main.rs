//! `sealion` command-line interface (spec §103).
//!
//! Subcommands are wired end-to-end as milestones land. Until then each
//! unimplemented subcommand prints a clear "not yet implemented" message
//! with the milestone that will deliver it, rather than failing obscurely.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

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
    if let Some(path) = &cli.config {
        let toml = std::fs::read_to_string(path)?;
        let cfg = sealion_core::config::Config::from_toml(&toml)?;
        tracing::info!(?path, "loaded config: {cfg:?}");
    }
    match cli.command {
        Command::Index(a) => match a.op {
            None => stub(2, &format!("index {}", a.path.unwrap_or_default())),
            Some(IndexOp::Stats) => stub(4, "index stats"),
            Some(IndexOp::Verify) => stub(4, "index verify"),
            Some(IndexOp::Merge) => stub(6, "index merge"),
        },
        Command::Crawl(_) => stub(11, "crawl"),
        Command::Search(a) => stub(
            if a.explain { 7 } else { 3 },
            &format!("search \"{}\"", a.query),
        ),
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
