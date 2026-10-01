//! Crawl orchestration (spec §6): seeds → frontier → fetch → extract →
//! documents, with depth/domain caps, retries, dedup, and resumability.
//!
//! The loop is sequential per fetch (politeness is per-host anyway) with a
//! global page budget; parallelism across hosts is milestone 13+ work.
//! Every accepted page becomes a [`sealion_core::document::Document`]
//! (`Source::Web`), so `sealion crawl` feeds the same segment pipeline as
//! local corpora: web → crawler → index → search.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sealion_core::config::CrawlerConfig;
use sealion_core::document::{DocId, Document, Source};

use crate::canonical::{canonicalize, host_of};
use crate::dedup::{fnv1a64, fnv1a64_hex, normalize_text, Dedup};
use crate::fetch::{FetchResult, Fetcher};
use crate::frontier::Frontier;
use crate::html::extract;

/// Outcome counters for one crawl run.
#[derive(Debug, Default, Clone)]
pub struct CrawlStats {
    pub pages: usize,
    pub failed: u64,
    pub retried: u64,
    pub robots_denied: u64,
    pub skipped_content: u64,
    pub exact_dups: u64,
    pub near_dups: u64,
}

impl CrawlStats {
    pub fn as_map(&self) -> HashMap<&'static str, u64> {
        HashMap::from([
            ("pages", self.pages as u64),
            ("failed", self.failed),
            ("retried", self.retried),
            ("robots_denied", self.robots_denied),
            ("skipped_content", self.skipped_content),
            ("exact_dups", self.exact_dups),
            ("near_dups", self.near_dups),
        ])
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A crawl run: owns frontier, fetcher, dedup, and seed-domain policy.
pub struct Crawler {
    config: CrawlerConfig,
    frontier: Frontier,
    fetcher: Fetcher,
    dedup: Dedup,
    seed_hosts: HashSet<String>,
    state_dir: PathBuf,
    stats: CrawlStats,
}

impl Crawler {
    pub fn new(config: CrawlerConfig, state_dir: PathBuf) -> Result<Self, String> {
        let fetcher = Fetcher::new(&config)?;
        let frontier = Frontier::load(&state_dir.join("frontier.json"))
            .map_err(|e| format!("frontier load: {e}"))?;
        Ok(Self {
            config,
            frontier,
            fetcher,
            dedup: Dedup::default(),
            seed_hosts: HashSet::new(),
            state_dir,
            stats: CrawlStats::default(),
        })
    }

    /// Seed URLs (canonicalized, domain-registered, frontier-queued).
    /// Returns the number accepted.
    pub fn add_seeds(&mut self, seeds: &[String]) -> usize {
        let mut n = 0;
        for s in seeds {
            let Some(canon) = canonicalize(s) else {
                continue;
            };
            let Some(host) = host_of(&canon) else {
                continue;
            };
            self.seed_hosts.insert(host.clone());
            if self.frontier.push(canon, host, 100, 0, "seed".into()) {
                n += 1;
            }
        }
        n
    }

    /// Register already-indexed content hashes so resumes skip them.
    pub fn seed_hashes(&mut self, hashes: impl IntoIterator<Item = String>) {
        for h in hashes {
            self.dedup.seed_exact(h);
        }
    }

    pub fn stats(&self) -> &CrawlStats {
        &self.stats
    }

    /// Run until the page budget is spent or the frontier drains.
    /// Returns accepted documents (stable IDs: FNV of canonical URL).
    pub async fn run(&mut self) -> Result<Vec<Document>, String> {
        let mut docs = Vec::new();
        let budget = self.config.max_pages;
        while docs.len() < budget {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let Some(entry) = self.frontier.pop_eligible(now_ms) else {
                break;
            };
            if entry.depth > self.config.max_depth {
                continue;
            }
            match self.fetcher.fetch(&entry.url).await {
                FetchResult::Html { final_url, body } => {
                    // Redirects re-canonicalize; dedup the frontier key.
                    let canon = canonicalize(&final_url).unwrap_or(final_url.clone());
                    if let Some((doc, outlinks)) = self.accept(&canon, &entry.source, &body) {
                        // Queue outlinks within budget/depth/policy.
                        self.queue_links(&outlinks, entry.depth + 1, &canon);
                        docs.push(doc);
                        self.stats.pages += 1;
                    }
                    let _ = self.frontier.save();
                }
                FetchResult::SkippedContentType(_) => {
                    self.stats.skipped_content += 1;
                }
                FetchResult::Denied(_) => {
                    self.stats.robots_denied += 1;
                }
                FetchResult::Retryable(e) => {
                    if entry.retries < self.config.max_retries {
                        let backoff = self.config.retry_backoff_ms << entry.retries.min(4);
                        self.frontier.retry_later(entry, backoff);
                        self.stats.retried += 1;
                    } else {
                        self.stats.failed += 1;
                        tracing::warn!(error = %e, "dropping url after retries");
                    }
                }
                FetchResult::Failed(e) => {
                    self.stats.failed += 1;
                    tracing::warn!(url = %entry.url, error = %e, "fetch failed");
                }
            }
            if docs.len() % 25 == 0 {
                let _ = self.frontier.save();
            }
        }
        let _ = self.frontier.save();
        // Persist dedup hashes for the next resume (exact set only).
        Ok(docs)
    }

    /// Extract, dedup, and build a Document plus its outlinks.
    /// None = duplicate.
    fn accept(&mut self, url: &str, source: &str, html: &str) -> Option<(Document, Vec<String>)> {
        let ext = extract(url, html);
        // Image alt text joins the body (visible content); srcs recorded
        // for the UI. Binaries themselves are never fetched (§11).
        let mut body = ext.body;
        if !ext.image_alts.is_empty() {
            body.push_str("\n\n");
            body.push_str(&ext.image_alts.join("\n"));
        }
        let normalized = normalize_text(&format!(
            "{}\n{}\n{}",
            ext.title,
            ext.headings.join("\n"),
            body
        ));
        // Exact check first (cheap), then SimHash near-dup.
        let hex = fnv1a64_hex(normalized.as_bytes());
        if self.dedup.check(&normalized) {
            let s = self.dedup.stats();
            self.stats.exact_dups = s["exact_dups"];
            self.stats.near_dups = s["near_dups"];
            return None;
        }
        let id = DocId(fnv1a64(url.as_bytes()));
        let mut metadata = HashMap::new();
        metadata.insert("crawl_source".to_string(), source.to_string());
        // Outlink graph for PageRank authority (milestone 18). Newline-
        // joined (URLs never contain raw newlines); capped at 64 targets.
        let outlink_targets: Vec<String> =
            ext.links.iter().map(|(u, _)| u.clone()).take(64).collect();
        if !outlink_targets.is_empty() {
            metadata.insert("outlinks".to_string(), outlink_targets.join("\n"));
        }
        if !ext.image_srcs.is_empty() {
            metadata.insert("image_refs".to_string(), ext.image_srcs.join(","));
            metadata.insert("image_count".to_string(), ext.image_srcs.len().to_string());
        }
        if let Some(desc) = ext.description {
            metadata.insert("description".to_string(), desc);
        }
        let anchor_text = ext
            .links
            .iter()
            .filter(|(_, a)| !a.is_empty())
            .map(|(_, a)| a.clone())
            .take(16)
            .collect();
        let outlinks = ext.links.iter().map(|(u, _)| u.clone()).collect();
        let doc = Document {
            id,
            source: Source::Web {
                url: url.to_string(),
            },
            url: url.to_string(),
            title: if ext.title.is_empty() {
                url.to_string()
            } else {
                ext.title
            },
            headings: ext.headings,
            body,
            anchor_text,
            metadata,
            timestamp: now_secs(),
            language: "en".to_string(),
            content_hash: hex,
        };
        Some((doc, outlinks))
    }

    /// Queue outlinks: same-host or seed-domain only (per
    /// `restrict_to_seed_domains`), lower priority than seeds.
    fn queue_links(&mut self, outlinks: &[String], depth: usize, from: &str) {
        if depth > self.config.max_depth {
            return;
        }
        for link in outlinks {
            let Some(host) = host_of(link) else { continue };
            if self.config.restrict_to_seed_domains && !self.seed_hosts.contains(&host) {
                continue;
            }
            // Deprioritize deep pages so breadth wins early.
            let priority = 50u32.saturating_sub(depth as u32 * 10);
            self.frontier
                .push(link.clone(), host, priority, depth, format!("link:{from}"));
        }
    }

    pub fn frontier_path(&self) -> PathBuf {
        self.state_dir.join("frontier.json")
    }
}
