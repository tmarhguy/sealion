//! Polite fetching (spec §8, §102): timeouts, redirect limits, response
//! size caps, gzip (via reqwest), per-host politeness, robots cache, and
//! SSRF defense (non-global IPs rejected unless `allow_loopback`).
//!
//! reqwest is the HTTP transport only. Scheduling, robots parsing,
//! canonicalization, and size enforcement are ours.

use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use sealion_core::config::CrawlerConfig;

use crate::robots::{Robots, RobotsCache};

/// Per-host politeness state.
#[derive(Debug)]
struct HostState {
    next_eligible: Instant,
    inflight: usize,
}

/// Fetch outcome.
#[derive(Debug)]
pub enum FetchResult {
    /// HTML body (non-HTML content types are reported as skipped).
    Html {
        final_url: String,
        body: String,
    },
    SkippedContentType(String),
    Denied(String),
    Retryable(String),
    Failed(String),
}

pub struct Fetcher {
    client: reqwest::Client,
    config: CrawlerConfig,
    hosts: HashMap<String, HostState>,
    robots: RobotsCache,
}

impl Fetcher {
    pub fn new(config: &CrawlerConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.fetch_timeout_ms))
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects))
            .gzip(true)
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            config: config.clone(),
            hosts: HashMap::new(),
            robots: RobotsCache::new(),
        })
    }

    fn host_state(&mut self, host: &str) -> &mut HostState {
        self.hosts.entry(host.to_string()).or_insert(HostState {
            next_eligible: Instant::now(),
            inflight: 0,
        })
    }

    /// Wait until `host` is polite to fetch from (crawl delay + robots
    /// delay, whichever is larger). Returns the applied delay.
    pub async fn wait_polite(&mut self, host: &str) {
        let delay = self.config.crawl_delay_ms;
        let robots_delay = self
            .robots
            .get(host)
            .and_then(|r| r.crawl_delay_ms)
            .unwrap_or(0);
        let need = delay.max(robots_delay);
        // Borrow dance: compute first, sleep after.
        let wait = {
            let st = self.host_state(host);
            let now = Instant::now();
            if st.next_eligible > now {
                st.next_eligible - now
            } else {
                Duration::ZERO
            }
            .max(Duration::ZERO)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let st = self.host_state(host);
        st.inflight += 1;
        st.next_eligible = Instant::now() + Duration::from_millis(need);
    }

    fn release(&mut self, host: &str) {
        if let Some(st) = self.hosts.get_mut(host) {
            st.inflight = st.inflight.saturating_sub(1);
        }
    }

    /// Resolve `host` and reject non-global IPs unless loopback is allowed
    /// (test-only escape hatch; production default denies).
    fn ssrf_check(&self, host: &str) -> Result<(), String> {
        // Literal IPs first.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return self.ip_allowed(ip);
        }
        // DNS: every resolved address must be allowed.
        match (host, 80).to_socket_addrs() {
            Ok(addrs) => {
                let mut any = false;
                for a in addrs {
                    any = true;
                    self.ip_allowed(a.ip())?;
                }
                if !any {
                    return Err(format!("no addresses for {host}"));
                }
                Ok(())
            }
            Err(e) => Err(format!("dns failed for {host}: {e}")),
        }
    }

    fn ip_allowed(&self, ip: IpAddr) -> Result<(), String> {
        if self.config.allow_loopback {
            return Ok(());
        }
        // Deny loopback, private, link-local, multicast, unspecified.
        let denied = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_multicast()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.is_documentation()
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_multicast() || v6.is_unspecified(),
        };
        if denied {
            return Err(format!("SSRF defense: refusing to fetch {ip}"));
        }
        Ok(())
    }

    async fn ensure_robots(
        &mut self,
        scheme: &str,
        host: &str,
        port: Option<u16>,
    ) -> Result<(), String> {
        if !self.config.robots_enabled || self.robots.get(host).is_some() {
            return Ok(());
        }
        let authority = match port {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        let url = format!("{scheme}://{authority}/robots.txt");
        let body = match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
            _ => String::new(), // Unreachable robots = allow all.
        };
        let robots = Robots::parse(&body, &self.config.user_agent);
        self.robots.insert(host.to_string(), robots);
        Ok(())
    }

    /// Fetch one canonical URL. Enforces robots, politeness, SSRF, size.
    pub async fn fetch(&mut self, url: &str) -> FetchResult {
        let parsed = match url::Url::parse(url) {
            Ok(u) => u,
            Err(e) => return FetchResult::Failed(format!("bad url: {e}")),
        };
        let scheme = parsed.scheme().to_string();
        let host = match parsed.host_str() {
            Some(h) => h.to_lowercase(),
            None => return FetchResult::Failed("no host".into()),
        };
        if let Err(e) = self.ssrf_check(&host) {
            return FetchResult::Denied(e);
        }
        if let Err(e) = self.ensure_robots(&scheme, &host, parsed.port()).await {
            return FetchResult::Failed(e);
        }
        let path = {
            let p = parsed.path().to_string();
            match parsed.query() {
                Some(q) => format!("{p}?{q}"),
                None => p,
            }
        };
        if self.config.robots_enabled && self.robots.get(&host).is_some_and(|r| !r.allowed(&path)) {
            return FetchResult::Denied("robots disallow".into());
        }
        self.wait_polite(&host).await;
        let result = self.fetch_inner(url).await;
        self.release(&host);
        result
    }

    async fn fetch_inner(&self, url: &str) -> FetchResult {
        let resp = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() || e.is_connect() => {
                return FetchResult::Retryable(e.to_string())
            }
            Err(e) => return FetchResult::Failed(e.to_string()),
        };
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return FetchResult::Retryable("HTTP 429".into());
        }
        if status.is_server_error() {
            return FetchResult::Retryable(format!("HTTP {status}"));
        }
        if !status.is_success() {
            return FetchResult::Failed(format!("HTTP {status}"));
        }
        // Content-type gate before reading the body.
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let is_html = ctype.is_empty() || ctype.contains("html") || ctype.contains("text/");
        if !is_html {
            // Images, PDFs, binaries: never fetched for indexing (§11).
            return FetchResult::SkippedContentType(ctype);
        }
        // Size bound: content-length pre-check, then post-read cap.
        // (True streaming truncation would need chunked reads; the fetch
        // timeout bounds pathological streams. Documented limitation.)
        let cap = self.config.max_response_bytes;
        if let Some(len) = resp.content_length() {
            if len > cap as u64 {
                return FetchResult::Failed(format!("response too large: {len} bytes"));
            }
        }
        let final_url = resp.url().to_string();
        match resp.bytes().await {
            Ok(bytes) => {
                if bytes.len() > cap {
                    return FetchResult::Failed(format!(
                        "response too large: {} bytes",
                        bytes.len()
                    ));
                }
                match String::from_utf8(bytes.to_vec()) {
                    Ok(body) => FetchResult::Html { final_url, body },
                    Err(_) => {
                        // Lossy fallback for mislabeled charsets.
                        FetchResult::Html {
                            final_url,
                            body: String::from_utf8_lossy(bytes.as_ref()).into_owned(),
                        }
                    }
                }
            }
            Err(e) => FetchResult::Failed(e.to_string()),
        }
    }
}
