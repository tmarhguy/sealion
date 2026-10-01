//! robots.txt parsing and matching (spec §8).
//!
//! Implements the operative subset of RFC 9309:
//!
//! - `User-agent` groups (case-insensitive substring match on our agent,
//!   plus `*` fallback; most-specific group wins, first-group-wins ties);
//! - `Allow` / `Disallow` path prefixes, longest-match wins (Allow wins
//!   exact ties), empty Disallow value allows all;
//! - `$` end-anchor and `*` wildcard in paths;
//! - `Crawl-delay` (seconds, may be fractional → rounded to ms).
//!
//! Anything unparseable is treated as "allow all" (fail-open keeps the
//! crawler useful against malformed files; fetch failures are separate).

use std::collections::HashMap;

/// One Allow/Disallow rule.
#[derive(Debug, Clone)]
struct Rule {
    allow: bool,
    pattern: String,
}

/// Parsed robots file for one host.
#[derive(Debug, Clone, Default)]
pub struct Robots {
    rules: Vec<Rule>,
    pub crawl_delay_ms: Option<u64>,
}

impl Robots {
    /// Parse a robots.txt body for `agent` (already lowercased matching).
    pub fn parse(body: &str, agent: &str) -> Self {
        let agent = agent.to_lowercase();
        // Group consecutive rules by preceding User-agent lines.
        let mut groups: Vec<(Vec<String>, Vec<Rule>, Option<u64>)> = Vec::new();
        let mut current_agents: Vec<String> = Vec::new();
        let mut current_rules: Vec<Rule> = Vec::new();
        let mut current_delay: Option<u64> = None;
        let mut in_group = false;
        let flush = |agents: &mut Vec<String>,
                     rules: &mut Vec<Rule>,
                     delay: &mut Option<u64>,
                     groups: &mut Vec<(Vec<String>, Vec<Rule>, Option<u64>)>| {
            if !agents.is_empty() || !rules.is_empty() {
                groups.push((std::mem::take(agents), std::mem::take(rules), *delay));
                *delay = None;
            }
        };
        for raw in body.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            match k.trim().to_lowercase().as_str() {
                "user-agent" => {
                    if in_group {
                        flush(
                            &mut current_agents,
                            &mut current_rules,
                            &mut current_delay,
                            &mut groups,
                        );
                        in_group = false;
                    }
                    current_agents.push(v.trim().to_lowercase());
                }
                "allow" | "disallow" => {
                    in_group = true;
                    let pat = v.trim().to_string();
                    if k.trim().eq_ignore_ascii_case("disallow") && pat.is_empty() {
                        continue; // Empty Disallow = allow all (no rule needed).
                    }
                    current_rules.push(Rule {
                        allow: k.trim().eq_ignore_ascii_case("allow"),
                        pattern: pat,
                    });
                }
                "crawl-delay" => {
                    in_group = true;
                    if let Ok(secs) = v.trim().parse::<f64>() {
                        if secs >= 0.0 {
                            current_delay = Some((secs * 1000.0).round() as u64);
                        }
                    }
                }
                _ => {}
            }
        }
        flush(
            &mut current_agents,
            &mut current_rules,
            &mut current_delay,
            &mut groups,
        );

        // Pick the most specific matching group: longest agent substring
        // match wins; `*` matches everything with specificity 0.
        let mut best: Option<(usize, usize)> = None; // (specificity, idx)
        for (i, (agents, _, _)) in groups.iter().enumerate() {
            for a in agents {
                let spec = if a == "*" {
                    Some(0)
                } else if agent.contains(a.as_str()) || a.contains(agent.as_str()) {
                    Some(a.len())
                } else {
                    None
                };
                if let Some(s) = spec {
                    if best.is_none_or(|(bs, _)| s > bs) {
                        best = Some((s, i));
                    }
                }
            }
        }
        match best {
            Some((_, i)) => Robots {
                rules: groups[i].1.clone(),
                crawl_delay_ms: groups[i].2,
            },
            None => Robots::default(), // No matching group → allow all.
        }
    }

    /// Whether `path` (path + optional query) may be fetched.
    pub fn allowed(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for r in &self.rules {
            if path_matches(&r.pattern, path) {
                let len = r.pattern.len();
                // Longest match wins; Allow wins exact ties.
                match best {
                    None => best = Some((len, r.allow)),
                    Some((bl, ba)) => {
                        if len > bl || (len == bl && r.allow && !ba) {
                            best = Some((len, r.allow));
                        }
                    }
                }
            }
        }
        best.map(|(_, allow)| allow).unwrap_or(true)
    }
}

/// Path pattern match with `*` (any run) and `$` (end anchor).
fn path_matches(pattern: &str, path: &str) -> bool {
    let (pat, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    if !pat.contains('*') {
        return if anchored {
            path == pat
        } else {
            path.starts_with(pat)
        };
    }
    // Wildcard: split into literals, all must appear in order.
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0;
    // Leading literal must anchor at start (unless pattern starts with *).
    if !pat.starts_with('*') {
        if !path.starts_with(parts[0]) {
            return false;
        }
        pos = parts[0].len();
    }
    let last_idx = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 && !pat.starts_with('*') {
            continue; // Already matched above.
        }
        match path[pos..].find(part) {
            Some(rel) => {
                pos += rel + part.len();
                if anchored && i == last_idx && pos != path.len() {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// In-memory per-host robots cache with fetch-once semantics.
#[derive(Debug, Default)]
pub struct RobotsCache {
    entries: HashMap<String, Robots>,
}

impl RobotsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, host: &str) -> Option<&Robots> {
        self.entries.get(host)
    }

    pub fn insert(&mut self, host: String, robots: Robots) {
        self.entries.insert(host, robots);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "
        User-agent: *
        Disallow: /private/
        Disallow: /tmp

        User-agent: SeaLionBot
        Disallow: /secret
        Allow: /secret/public
        Crawl-delay: 2
    ";

    #[test]
    fn picks_most_specific_group() {
        let r = Robots::parse(SAMPLE, "SeaLionBot/0.1");
        // SeaLionBot group applies, not *.
        assert!(r.allowed("/private/page"));
        assert!(!r.allowed("/secret/page"));
        assert!(r.allowed("/secret/public/page"));
        assert_eq!(r.crawl_delay_ms, Some(2000));
    }

    #[test]
    fn star_group_applies_to_others() {
        let r = Robots::parse(SAMPLE, "OtherBot");
        assert!(!r.allowed("/private/page"));
        assert!(r.allowed("/secret/page"));
        assert_eq!(r.crawl_delay_ms, None);
    }

    #[test]
    fn longest_match_and_allow_tiebreak() {
        let r = Robots::parse("User-agent: *\nDisallow: /a\nAllow: /a/b", "x");
        assert!(!r.allowed("/a/c"));
        assert!(r.allowed("/a/b/c"));
    }

    #[test]
    fn wildcards_and_anchors() {
        let r = Robots::parse("User-agent: *\nDisallow: /*.php$\nAllow: /public/*", "x");
        assert!(!r.allowed("/index.php"));
        assert!(r.allowed("/index.php/bak"));
        assert!(r.allowed("/public/x.php"));
    }

    #[test]
    fn malformed_is_fail_open() {
        let r = Robots::parse("this is not robots", "x");
        assert!(r.allowed("/anything"));
    }
}
