//! URL canonicalization (spec §9).
//!
//! Conservative normalization only — merges obvious duplicate forms, never
//! genuinely distinct pages:
//!
//! - lowercase scheme and host;
//! - strip default ports (80/443);
//! - drop fragments;
//! - resolve `.`/`..` dot segments (via `url`);
//! - empty path becomes `/`;
//! - keep query strings byte-identical (no param stripping: tracking params
//!   *can* change content; dedup handles true duplicates later).
//!
//! Anything unparseable is rejected (`None`) before it enters the frontier.

use url::Url;

/// Canonicalize `raw` against no base (absolute URLs only).
/// Returns the canonical string, or `None` if unparseable or a non-web,
/// non-file scheme. `file` is accepted for local corpora (the crawler
/// itself only fetches http(s); see `host_of` returning `None` there).
pub fn canonicalize(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw.trim()).ok()?;
    match url.scheme() {
        "http" | "https" | "file" => {}
        _ => return None,
    }
    // url crate already lowercases scheme/host, resolves dot segments,
    // and drops fragments only if we clear them: do that explicitly.
    url.set_fragment(None);
    // Strip default ports.
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        let _ = url.set_port(None);
    }
    // Normalize empty path to "/" so `https://h` and `https://h/` merge.
    if url.path().is_empty() {
        url.set_path("/");
    }
    Some(url.as_str().to_string())
}

/// Host of a canonical URL (lowercased, no port).
pub fn host_of(canonical: &str) -> Option<String> {
    Url::parse(canonical)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
}

/// Resolve `href` against page `base`, then canonicalize.
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("javascript:")
        || href.starts_with("mailto:")
        || href.starts_with("data:")
    {
        return None;
    }
    let base_url = Url::parse(base).ok()?;
    let joined = base_url.join(href).ok()?;
    canonicalize(joined.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_obvious_duplicates() {
        assert_eq!(
            canonicalize("HTTP://Example.COM:80/a/./b/../c#frag"),
            canonicalize("http://example.com/a/c")
        );
        assert_eq!(
            canonicalize("https://example.com:443/"),
            Some("https://example.com/".into())
        );
        assert_eq!(
            canonicalize("https://example.com"),
            Some("https://example.com/".into())
        );
    }

    #[test]
    fn keeps_distinct_pages_distinct() {
        assert_ne!(
            canonicalize("https://example.com/a?x=1"),
            canonicalize("https://example.com/a?x=2")
        );
        assert_ne!(
            canonicalize("https://example.com/a"),
            canonicalize("https://example.com/b")
        );
    }

    #[test]
    fn rejects_non_http() {
        assert_eq!(canonicalize("ftp://example.com/x"), None);
        assert_eq!(canonicalize("not a url"), None);
        assert_eq!(resolve("https://example.com/a", "javascript:void(0)"), None);
        assert_eq!(resolve("https://example.com/a", "#frag"), None);
    }

    #[test]
    fn resolves_relative_links() {
        assert_eq!(
            resolve("https://example.com/dir/page", "../other"),
            Some("https://example.com/other".into())
        );
        assert_eq!(
            resolve("https://example.com/dir/page", "/abs"),
            Some("https://example.com/abs".into())
        );
    }
}
