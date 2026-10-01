//! Engine-wide configuration (spec §104).
//!
//! Loaded from TOML. Every knob must be validated: invalid values fail fast
//! at startup, never silently.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Configuration error: which key was invalid and why.
#[derive(Debug, Error)]
#[error("invalid config: {key}: {reason}")]
pub struct ConfigError {
    pub key: String,
    pub reason: String,
}

/// Top-level SeaLion configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub index: IndexConfig,
    #[serde(default)]
    pub ranking: RankingConfig,
    #[serde(default)]
    pub crawler: CrawlerConfig,
    #[serde(default)]
    pub query: QueryConfig,
    #[serde(default)]
    pub cluster: ClusterConfig,
    #[serde(default)]
    pub analysis: AnalysisConfig,
}

impl Config {
    /// Parse TOML and validate. Fails on unknown keys or invalid values.
    pub fn from_toml(toml: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml_parse(toml)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate every knob. Add a check here when adding a knob.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let check = |key: &str, ok: bool, reason: &str| {
            if ok {
                Ok(())
            } else {
                Err(ConfigError {
                    key: key.to_string(),
                    reason: reason.to_string(),
                })
            }
        };
        check(
            "index.segment_target_mb",
            self.index.segment_target_mb >= 1 && self.index.segment_target_mb <= 4096,
            "must be between 1 and 4096 MB",
        )?;
        check(
            "ranking.bm25_k1",
            self.ranking.bm25_k1 > 0.0 && self.ranking.bm25_k1 <= 10.0,
            "must be in (0, 10]",
        )?;
        check(
            "ranking.bm25_b",
            (0.0..=1.0).contains(&self.ranking.bm25_b),
            "must be in [0, 1]",
        )?;
        check(
            "crawler.max_connections",
            self.crawler.max_connections >= 1 && self.crawler.max_connections <= 4096,
            "must be between 1 and 4096",
        )?;
        check(
            "crawler.crawl_delay_ms",
            self.crawler.crawl_delay_ms <= 60_000,
            "must be at most 60000 ms",
        )?;
        check(
            "crawler.max_pages",
            self.crawler.max_pages >= 1 && self.crawler.max_pages <= 10_000_000,
            "must be between 1 and 10_000_000",
        )?;
        check(
            "crawler.max_retries",
            self.crawler.max_retries <= 10,
            "must be at most 10",
        )?;
        check(
            "crawler.retry_backoff_ms",
            self.crawler.retry_backoff_ms <= 300_000,
            "must be at most 300000 ms",
        )?;
        check(
            "crawler.fetch_timeout_ms",
            (100..=300_000).contains(&self.crawler.fetch_timeout_ms),
            "must be between 100 and 300000 ms",
        )?;
        check(
            "crawler.max_response_bytes",
            (1024..=1024 * 1024 * 1024).contains(&self.crawler.max_response_bytes),
            "must be between 1 KiB and 1 GiB",
        )?;
        check(
            "crawler.max_redirects",
            self.crawler.max_redirects <= 20,
            "must be at most 20",
        )?;
        check(
            "crawler.max_inflight_per_host",
            self.crawler.max_inflight_per_host >= 1 && self.crawler.max_inflight_per_host <= 64,
            "must be between 1 and 64",
        )?;
        check(
            "crawler.user_agent",
            !self.crawler.user_agent.trim().is_empty() && self.crawler.user_agent.len() <= 256,
            "must be a non-empty string of at most 256 chars",
        )?;
        check(
            "query.top_k",
            self.query.top_k >= 1 && self.query.top_k <= 10_000,
            "must be between 1 and 10000",
        )?;
        check(
            "cluster.shard_count",
            self.cluster.shard_count >= 1 && self.cluster.shard_count <= 1024,
            "must be between 1 and 1024",
        )?;
        check(
            "cluster.replication_factor",
            self.cluster.replication_factor >= 1 && self.cluster.replication_factor <= 8,
            "must be between 1 and 8",
        )?;
        self.analysis.validate()?;
        Ok(())
    }
}

fn toml_parse(toml: &str) -> Result<Config, ConfigError> {
    // Minimal TOML subset parser: enough for our flat [section]/key = value
    // config today. Replaced by the `toml` crate once networking allows
    // vendoring a new dependency cleanly. Documented limitation.
    let mut cfg = Config::default();
    let mut section = String::new();
    for (lineno, raw) in toml.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let (key, value) = raw.split_once('=').ok_or_else(|| ConfigError {
            key: format!("line {}", lineno + 1),
            reason: "expected `key = value`".to_string(),
        })?;
        let key = key.trim();
        let value = value.trim();
        let full = format!("{section}.{key}");
        let set_f64 = |slot: &mut f64| -> Result<(), ConfigError> {
            *slot = value.parse::<f64>().map_err(|_| ConfigError {
                key: full.clone(),
                reason: "expected a number".to_string(),
            })?;
            Ok(())
        };
        let set_u64 = |slot: &mut u64| -> Result<(), ConfigError> {
            *slot = value.parse::<u64>().map_err(|_| ConfigError {
                key: full.clone(),
                reason: "expected an integer".to_string(),
            })?;
            Ok(())
        };
        let set_u32 = |slot: &mut u32| -> Result<(), ConfigError> {
            *slot = value.parse::<u32>().map_err(|_| ConfigError {
                key: full.clone(),
                reason: "expected an integer".to_string(),
            })?;
            Ok(())
        };
        let set_usize = |slot: &mut usize| -> Result<(), ConfigError> {
            *slot = value.parse::<usize>().map_err(|_| ConfigError {
                key: full.clone(),
                reason: "expected an integer".to_string(),
            })?;
            Ok(())
        };
        let set_bool = |slot: &mut bool| -> Result<(), ConfigError> {
            *slot = match value {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(ConfigError {
                        key: full.clone(),
                        reason: "expected `true` or `false`".to_string(),
                    })
                }
            };
            Ok(())
        };
        let set_string = |slot: &mut String| -> Result<(), ConfigError> {
            let v = value.strip_prefix('"').and_then(|s| s.strip_suffix('"'));
            *slot = v
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.to_string());
            Ok(())
        };
        let set_stemmer = |slot: &mut StemmerKind| -> Result<(), ConfigError> {
            // Accept both `porter` and `"porter"` (TOML strings may be quoted).
            let raw = value
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(value);
            *slot = raw.parse::<StemmerKind>().map_err(|_| ConfigError {
                key: full.clone(),
                reason: "expected `none` or `porter`".to_string(),
            })?;
            Ok(())
        };
        match full.as_str() {
            "index.segment_target_mb" => set_usize(&mut cfg.index.segment_target_mb)?,
            "ranking.bm25_k1" => set_f64(&mut cfg.ranking.bm25_k1)?,
            "ranking.bm25_b" => set_f64(&mut cfg.ranking.bm25_b)?,
            "ranking.title_weight" => set_f64(&mut cfg.ranking.title_weight)?,
            "ranking.heading_weight" => set_f64(&mut cfg.ranking.heading_weight)?,
            "ranking.body_weight" => set_f64(&mut cfg.ranking.body_weight)?,
            "ranking.anchor_weight" => set_f64(&mut cfg.ranking.anchor_weight)?,
            "crawler.max_connections" => set_usize(&mut cfg.crawler.max_connections)?,
            "crawler.crawl_delay_ms" => set_u64(&mut cfg.crawler.crawl_delay_ms)?,
            "crawler.max_depth" => set_usize(&mut cfg.crawler.max_depth)?,
            "crawler.max_pages" => set_usize(&mut cfg.crawler.max_pages)?,
            "crawler.max_retries" => set_u32(&mut cfg.crawler.max_retries)?,
            "crawler.retry_backoff_ms" => set_u64(&mut cfg.crawler.retry_backoff_ms)?,
            "crawler.fetch_timeout_ms" => set_u64(&mut cfg.crawler.fetch_timeout_ms)?,
            "crawler.max_response_bytes" => set_usize(&mut cfg.crawler.max_response_bytes)?,
            "crawler.max_redirects" => set_usize(&mut cfg.crawler.max_redirects)?,
            "crawler.max_inflight_per_host" => set_usize(&mut cfg.crawler.max_inflight_per_host)?,
            "crawler.robots_enabled" => set_bool(&mut cfg.crawler.robots_enabled)?,
            "crawler.allow_loopback" => set_bool(&mut cfg.crawler.allow_loopback)?,
            "crawler.restrict_to_seed_domains" => {
                set_bool(&mut cfg.crawler.restrict_to_seed_domains)?
            }
            "crawler.user_agent" => set_string(&mut cfg.crawler.user_agent)?,
            "query.top_k" => set_usize(&mut cfg.query.top_k)?,
            "query.max_query_terms" => set_usize(&mut cfg.query.max_query_terms)?,
            "cluster.shard_count" => set_usize(&mut cfg.cluster.shard_count)?,
            "cluster.replication_factor" => set_usize(&mut cfg.cluster.replication_factor)?,
            "analysis.stop_words" => set_bool(&mut cfg.analysis.stop_words)?,
            "analysis.stemmer" => set_stemmer(&mut cfg.analysis.stemmer)?,
            "analysis.min_token_len" => set_usize(&mut cfg.analysis.min_token_len)?,
            "analysis.title.stop_words" => {
                let mut v = false;
                set_bool(&mut v)?;
                cfg.analysis.title.stop_words = Some(v);
            }
            "analysis.title.stemmer" => {
                let mut v = StemmerKind::None;
                set_stemmer(&mut v)?;
                cfg.analysis.title.stemmer = Some(v);
            }
            "analysis.title.min_token_len" => {
                let mut v = 0;
                set_usize(&mut v)?;
                cfg.analysis.title.min_token_len = Some(v);
            }
            "analysis.heading.stop_words" => {
                let mut v = false;
                set_bool(&mut v)?;
                cfg.analysis.heading.stop_words = Some(v);
            }
            "analysis.heading.stemmer" => {
                let mut v = StemmerKind::None;
                set_stemmer(&mut v)?;
                cfg.analysis.heading.stemmer = Some(v);
            }
            "analysis.heading.min_token_len" => {
                let mut v = 0;
                set_usize(&mut v)?;
                cfg.analysis.heading.min_token_len = Some(v);
            }
            "analysis.body.stop_words" => {
                let mut v = false;
                set_bool(&mut v)?;
                cfg.analysis.body.stop_words = Some(v);
            }
            "analysis.body.stemmer" => {
                let mut v = StemmerKind::None;
                set_stemmer(&mut v)?;
                cfg.analysis.body.stemmer = Some(v);
            }
            "analysis.body.min_token_len" => {
                let mut v = 0;
                set_usize(&mut v)?;
                cfg.analysis.body.min_token_len = Some(v);
            }
            "analysis.anchor.stop_words" => {
                let mut v = false;
                set_bool(&mut v)?;
                cfg.analysis.anchor.stop_words = Some(v);
            }
            "analysis.anchor.stemmer" => {
                let mut v = StemmerKind::None;
                set_stemmer(&mut v)?;
                cfg.analysis.anchor.stemmer = Some(v);
            }
            "analysis.anchor.min_token_len" => {
                let mut v = 0;
                set_usize(&mut v)?;
                cfg.analysis.anchor.min_token_len = Some(v);
            }
            _ => {
                return Err(ConfigError {
                    key: full,
                    reason: "unknown configuration key".to_string(),
                })
            }
        }
    }
    Ok(cfg)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    /// Target size of one immutable segment before flushing (§17–18).
    pub segment_target_mb: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            segment_target_mb: 256,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingConfig {
    pub bm25_k1: f64,
    pub bm25_b: f64,
    /// Per-field BM25 multipliers (§29). Tuned later via relevance eval (§53).
    pub title_weight: f64,
    pub heading_weight: f64,
    pub body_weight: f64,
    pub anchor_weight: f64,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            bm25_k1: 1.2,
            bm25_b: 0.75,
            title_weight: 4.0,
            heading_weight: 2.0,
            body_weight: 1.0,
            anchor_weight: 1.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlerConfig {
    pub max_connections: usize,
    /// Politeness delay between requests to the same host (§8).
    pub crawl_delay_ms: u64,
    /// Maximum link-following depth from seeds (§6).
    pub max_depth: usize,
    /// Hard cap on documents produced per crawl run (§6).
    pub max_pages: usize,
    /// Bounded retries per URL before it is reported failed (§6).
    pub max_retries: u32,
    /// Base backoff between retries; doubled per attempt (§6).
    pub retry_backoff_ms: u64,
    /// Total per-request timeout including body download (§102).
    pub fetch_timeout_ms: u64,
    /// Response body cap; larger bodies are aborted mid-stream (§102).
    pub max_response_bytes: usize,
    /// Maximum redirects followed per fetch (§102).
    pub max_redirects: usize,
    /// Per-host concurrent in-flight requests (§8 connection limits).
    pub max_inflight_per_host: usize,
    /// Whether robots.txt is fetched and respected (§8).
    pub robots_enabled: bool,
    /// TEST ONLY: allow fetching loopback/private IPs. Never enable in
    /// production — it disables the SSRF defense (§102).
    pub allow_loopback: bool,
    /// If true, only follow links whose host is a seed host or subdomain
    /// (§6 domain restrictions).
    pub restrict_to_seed_domains: bool,
    /// User-Agent header sent on every request (§8).
    pub user_agent: String,
}

impl Default for CrawlerConfig {
    fn default() -> Self {
        Self {
            max_connections: 128,
            crawl_delay_ms: 1000,
            max_depth: 3,
            max_pages: 10_000,
            max_retries: 3,
            retry_backoff_ms: 2000,
            fetch_timeout_ms: 15_000,
            max_response_bytes: 8 * 1024 * 1024,
            max_redirects: 5,
            max_inflight_per_host: 2,
            robots_enabled: true,
            allow_loopback: false,
            restrict_to_seed_domains: true,
            user_agent: "SeaLionBot/0.1 (+https://tmarhguy.com)".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryConfig {
    pub top_k: usize,
    /// Rejects pathological queries (§102 request complexity limits).
    pub max_query_terms: usize,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            top_k: 20,
            max_query_terms: 64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub shard_count: usize,
    pub replication_factor: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            shard_count: 4,
            replication_factor: 1,
        }
    }
}

/// Stemming algorithm applied after stop-word removal (spec §12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StemmerKind {
    /// No stemming: terms are indexed as-is after case normalization.
    None,
    /// Porter (1980) suffix-stripping stemmer, implemented in
    /// `sealion-index` with no external dependency (ADR 002).
    #[default]
    Porter,
}

impl std::str::FromStr for StemmerKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(StemmerKind::None),
            "porter" => Ok(StemmerKind::Porter),
            _ => Err(()),
        }
    }
}

/// Per-field overrides for the analysis pipeline (spec §15).
///
/// Every knob is `None` by default, meaning "inherit the global
/// `[analysis]` value". `Some(v)` overrides it for that field only, so e.g.
/// titles can keep stemming while bodies disable it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldAnalysisConfig {
    #[serde(default)]
    pub stop_words: Option<bool>,
    #[serde(default)]
    pub stemmer: Option<StemmerKind>,
    #[serde(default)]
    pub min_token_len: Option<usize>,
}

/// Text analysis configuration (spec §12): the full pipeline
/// normalization → tokenizer → case folding → stop words → stemming
/// is driven by these knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Drop English stop words (default true).
    pub stop_words: bool,
    /// Stemming algorithm (default Porter).
    pub stemmer: StemmerKind,
    /// Drop terms shorter than this many characters after stemming
    /// (default 1 = keep everything).
    pub min_token_len: usize,
    /// Per-field overrides (all inherit by default).
    #[serde(default)]
    pub title: FieldAnalysisConfig,
    #[serde(default)]
    pub heading: FieldAnalysisConfig,
    #[serde(default)]
    pub body: FieldAnalysisConfig,
    #[serde(default)]
    pub anchor: FieldAnalysisConfig,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            stop_words: true,
            stemmer: StemmerKind::Porter,
            min_token_len: 1,
            title: FieldAnalysisConfig::default(),
            heading: FieldAnalysisConfig::default(),
            body: FieldAnalysisConfig::default(),
            anchor: FieldAnalysisConfig::default(),
        }
    }
}

impl AnalysisConfig {
    /// Validate analysis knobs. Called from [`Config::validate`].
    pub fn validate(&self) -> Result<(), ConfigError> {
        let check_len = |key: &str, v: usize| {
            if (1..=64).contains(&v) {
                Ok(())
            } else {
                Err(ConfigError {
                    key: key.to_string(),
                    reason: "must be between 1 and 64".to_string(),
                })
            }
        };
        check_len("analysis.min_token_len", self.min_token_len)?;
        for (name, f) in [
            ("title", &self.title),
            ("heading", &self.heading),
            ("body", &self.body),
            ("anchor", &self.anchor),
        ] {
            if let Some(v) = f.min_token_len {
                check_len(&format!("analysis.{name}.min_token_len"), v)?;
            }
        }
        Ok(())
    }

    /// Effective stop-word flag for a field (override or global).
    pub fn stop_words_for(&self, field: crate::field::Field) -> bool {
        self.field_cfg(field).stop_words.unwrap_or(self.stop_words)
    }

    /// Effective stemmer for a field (override or global).
    pub fn stemmer_for(&self, field: crate::field::Field) -> StemmerKind {
        self.field_cfg(field).stemmer.unwrap_or(self.stemmer)
    }

    /// Effective minimum token length for a field (override or global).
    pub fn min_token_len_for(&self, field: crate::field::Field) -> usize {
        self.field_cfg(field)
            .min_token_len
            .unwrap_or(self.min_token_len)
    }

    fn field_cfg(&self, field: crate::field::Field) -> &FieldAnalysisConfig {
        match field {
            crate::field::Field::Title => &self.title,
            crate::field::Field::Heading => &self.heading,
            crate::field::Field::Body => &self.body,
            crate::field::Field::Anchor => &self.anchor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn parses_spec_example() {
        let cfg = Config::from_toml(
            "[index]\nsegment_target_mb = 256\n\n[ranking]\nbm25_k1 = 1.2\nbm25_b = 0.75\n\n[crawler]\nmax_connections = 128\n\n[query]\ntop_k = 20\n",
        )
        .unwrap();
        assert_eq!(cfg.index.segment_target_mb, 256);
        assert_eq!(cfg.ranking.bm25_k1, 1.2);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Config::from_toml("[ranking]\nbm25_b = 1.5\n").is_err());
        assert!(Config::from_toml("[query]\ntop_k = 0\n").is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::from_toml("[ranking]\nbm99 = 1.0\n").is_err());
    }

    #[test]
    fn analysis_defaults_are_sane() {
        let cfg = Config::default();
        assert!(cfg.analysis.stop_words);
        assert_eq!(cfg.analysis.stemmer, StemmerKind::Porter);
        assert_eq!(cfg.analysis.min_token_len, 1);
        // Per-field overrides inherit the global values.
        for f in crate::field::Field::ALL {
            assert!(cfg.analysis.stop_words_for(f));
            assert_eq!(cfg.analysis.stemmer_for(f), StemmerKind::Porter);
            assert_eq!(cfg.analysis.min_token_len_for(f), 1);
        }
    }

    #[test]
    fn analysis_knobs_parse() {
        let cfg = Config::from_toml(
            "[analysis]\nstop_words = false\nstemmer = \"none\"\nmin_token_len = 2\n\n\
             [analysis.title]\nstemmer = \"porter\"\n",
        )
        .unwrap();
        assert!(!cfg.analysis.stop_words);
        assert_eq!(cfg.analysis.stemmer, StemmerKind::None);
        assert_eq!(cfg.analysis.min_token_len, 2);
        // Title overrides stemmer but inherits stop_words = false.
        assert_eq!(
            cfg.analysis.stemmer_for(crate::field::Field::Title),
            StemmerKind::Porter
        );
        assert!(!cfg.analysis.stop_words_for(crate::field::Field::Title));
        assert_eq!(
            cfg.analysis.stemmer_for(crate::field::Field::Body),
            StemmerKind::None
        );
    }

    #[test]
    fn analysis_rejects_bad_values() {
        assert!(Config::from_toml("[analysis]\nmin_token_len = 0\n").is_err());
        assert!(Config::from_toml("[analysis]\nstemmer = \"snowball\"\n").is_err());
        assert!(Config::from_toml("[analysis]\nstop_words = yes\n").is_err());
    }
}
