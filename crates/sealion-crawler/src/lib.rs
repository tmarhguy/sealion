//! SeaLion crawler: frontier, fetch, politeness, parsing, dedup (spec §6–11).
//!
//! Pipeline: seed URLs → [`canonical`] normalization → [`frontier`]
//! scheduling → [`fetch`] (politeness, [`robots`], SSRF defense) →
//! [`html`] field extraction → [`dedup`] (exact + SimHash) → `Document`.
//! Orchestrated by [`crawl::Crawler`]; `sealion crawl` indexes the
//! resulting documents through the standard segment pipeline.

pub mod canonical;
pub mod crawl;
pub mod dedup;
pub mod fetch;
pub mod frontier;
pub mod html;
pub mod robots;

pub use crawl::{CrawlStats, Crawler};
