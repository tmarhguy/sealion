//! Persistent crawl frontier (spec §7).
//!
//! Tracks per URL: host, priority, depth, retries, next-eligible fetch
//! time, discovery source. Scheduling is priority-first with per-host
//! politeness enforced by the fetcher (`next_eligible` + crawl delay).
//! State persists as JSON (atomic temp-write + rename) so crawls resume.
//!
//! No single domain can starve others: `pop_eligible` round-robins hosts
//! by remembering the last-served host.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One frontier entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontierEntry {
    pub url: String,
    pub host: String,
    pub priority: u32,
    pub depth: usize,
    pub retries: u32,
    /// Earliest fetch time (unix ms).
    pub next_eligible_ms: u64,
    pub source: String,
}

/// Scheduling frontier: priority queue + dedup set + persistence.
#[derive(Debug, Serialize, Deserialize)]
pub struct Frontier {
    /// Ready entries ordered by (priority desc, next_eligible, url).
    /// Stored as a sorted set of keys into `entries`.
    pending: BTreeSet<(u32, u64, String)>,
    entries: HashMap<String, FrontierEntry>,
    seen: HashSet<String>,
    #[serde(default)]
    last_host: Option<String>,
    #[serde(skip, default = "PathBuf::new")]
    path: PathBuf,
}

impl Frontier {
    pub fn new(path: PathBuf) -> Self {
        Self {
            pending: BTreeSet::new(),
            entries: HashMap::new(),
            seen: HashSet::new(),
            last_host: None,
            path,
        }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut f: Frontier = serde_json::from_slice(&bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                f.path = path.to_path_buf();
                Ok(f)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new(path.to_path_buf())),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn key(e: &FrontierEntry) -> (u32, u64, String) {
        // Reverse priority for BTreeSet ascending order (higher first).
        (u32::MAX - e.priority, e.next_eligible_ms, e.url.clone())
    }

    /// Queue a URL. Returns false if already seen (dedup at the frontier).
    pub fn push(
        &mut self,
        url: String,
        host: String,
        priority: u32,
        depth: usize,
        source: String,
    ) -> bool {
        if !self.seen.insert(url.clone()) {
            return false;
        }
        let e = FrontierEntry {
            url: url.clone(),
            host,
            priority,
            depth,
            retries: 0,
            next_eligible_ms: 0,
            source,
        };
        self.pending.insert(Self::key(&e));
        self.entries.insert(url, e);
        true
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn seen_count(&self) -> usize {
        self.seen.len()
    }

    /// Pop the best eligible entry: past its eligible time, preferring a
    /// different host than last served (domain fairness).
    pub fn pop_eligible(&mut self, now_ms_: u64) -> Option<FrontierEntry> {
        let key = self
            .pending
            .iter()
            .filter(|(_, eligible, _)| *eligible <= now_ms_)
            .find(|(_, _, url)| {
                let e = &self.entries[url];
                self.last_host.as_ref().is_none_or(|h| *h != e.host)
            })
            .or_else(|| {
                self.pending
                    .iter()
                    .find(|(_, eligible, _)| *eligible <= now_ms_)
            })
            .cloned()?;
        self.pending.remove(&key);
        let e = self.entries.remove(&key.2)?;
        self.last_host = Some(e.host.clone());
        Some(e)
    }

    /// Requeue after a retriable failure with backoff.
    pub fn retry_later(&mut self, mut entry: FrontierEntry, backoff_ms: u64) {
        entry.retries += 1;
        entry.next_eligible_ms = now_ms().saturating_add(backoff_ms);
        self.pending.insert(Self::key(&entry));
        self.entries.insert(entry.url.clone(), entry);
    }

    /// Next wake-up time (ms since epoch) if nothing is eligible now.
    pub fn next_wakeup_ms(&self) -> Option<u64> {
        self.pending.iter().map(|(_, eligible, _)| *eligible).next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontier() -> Frontier {
        Frontier::new(PathBuf::new())
    }

    #[test]
    fn dedups_and_prioritizes() {
        let mut f = frontier();
        assert!(f.push(
            "https://a.com/1".into(),
            "a.com".into(),
            1,
            0,
            "seed".into()
        ));
        assert!(!f.push(
            "https://a.com/1".into(),
            "a.com".into(),
            9,
            0,
            "seed".into()
        ));
        f.push(
            "https://b.com/1".into(),
            "b.com".into(),
            9,
            0,
            "seed".into(),
        );
        // Higher priority first.
        assert_eq!(f.pop_eligible(u64::MAX).unwrap().host, "b.com");
    }

    #[test]
    fn round_robins_hosts() {
        let mut f = frontier();
        f.push("https://a.com/1".into(), "a.com".into(), 5, 0, "s".into());
        f.push("https://a.com/2".into(), "a.com".into(), 5, 0, "s".into());
        f.push("https://b.com/1".into(), "b.com".into(), 5, 0, "s".into());
        // Same priority: first pop may be either host, second must differ.
        let first = f.pop_eligible(u64::MAX).unwrap().host;
        let second = f.pop_eligible(u64::MAX).unwrap().host;
        assert_ne!(first, second);
    }

    #[test]
    fn respects_eligible_time() {
        let mut f = frontier();
        f.push("https://a.com/1".into(), "a.com".into(), 5, 0, "s".into());
        assert!(f.pop_eligible(0).is_some());
        f.push("https://a.com/2".into(), "a.com".into(), 5, 0, "s".into());
        // Hack the entry's eligible time forward via retry.
        let mut e = f.pop_eligible(u64::MAX).unwrap();
        e.next_eligible_ms = u64::MAX;
        let key = Frontier::key(&e);
        f.pending.insert(key);
        f.entries.insert(e.url.clone(), e);
        assert!(f.pop_eligible(0).is_none());
    }

    #[test]
    fn persists_and_resumes() {
        let dir = std::env::temp_dir().join(format!("sealion-frontier-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frontier.json");
        let mut f = Frontier::new(path.clone());
        f.push(
            "https://a.com/1".into(),
            "a.com".into(),
            3,
            1,
            "seed".into(),
        );
        f.save().unwrap();
        let mut back = Frontier::load(&path).unwrap();
        assert_eq!(back.pending_count(), 1);
        assert_eq!(back.pop_eligible(u64::MAX).unwrap().depth, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
