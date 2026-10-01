//! HTML field extraction (spec §11).
//!
//! `scraper` provides the DOM only. Everything semantic is ours: title,
//! headings, body text with boilerplate suppression (`script`/`style`/
//! `nav`/`footer` subtrees dropped), link + anchor capture, canonical URL,
//! meta description, and **image handling** — binaries are never fetched
//! for indexing; `<img>` `alt` text joins the body and `src` values are
//! recorded in order (capped) for the UI layer.

use scraper::{Html, Selector};

/// Extracted page fields.
#[derive(Debug, Clone, Default)]
pub struct Extracted {
    pub title: String,
    pub headings: Vec<String>,
    pub body: String,
    pub links: Vec<(String, String)>,
    pub image_alts: Vec<String>,
    pub image_srcs: Vec<String>,
    pub canonical: Option<String>,
    pub description: Option<String>,
}

fn text_of(html: &Html, sel: &Selector) -> Vec<String> {
    html.select(sel)
        .map(|e| e.text().collect::<Vec<_>>().join(" "))
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract fields from `html` fetched at `page_url` (for link resolution).
pub fn extract(page_url: &str, html: &str) -> Extracted {
    let dom = Html::parse_document(html);
    let sel = |s: &str| Selector::parse(s).expect("static selector");
    let title_sel = sel("title");
    let h_sel = sel("h1, h2, h3, h4, h5, h6");
    let a_sel = sel("a[href]");
    let img_sel = sel("img");
    let canon_sel = sel("link[rel=canonical]");
    let meta_sel = sel("meta[name=description], meta[property='og:description']");

    let title = text_of(&dom, &title_sel)
        .into_iter()
        .next()
        .unwrap_or_default();
    let headings = text_of(&dom, &h_sel);

    // Body: drop boilerplate subtrees by removing them from a cloned DOM.
    // (scraper DOMs are immutable; instead, collect text while skipping any
    // text node with a script/style/nav/footer/header ancestor.)
    let skip_sel = sel("script, style, nav, footer, header, noscript, svg");
    let mut skip_ids = std::collections::HashSet::new();
    for e in dom.select(&skip_sel) {
        // Mark the element and its entire subtree as skipped.
        for d in e.descendants() {
            skip_ids.insert(d.id());
        }
    }
    let mut body_parts = Vec::new();
    // scraper 0.24: traverse all nodes; keep text whose ancestors are clean.
    for node in dom.tree.nodes() {
        if let Some(text) = node.value().as_text() {
            let mut cur = node.parent();
            let mut skip = false;
            while let Some(p) = cur {
                if skip_ids.contains(&p.id()) {
                    skip = true;
                    break;
                }
                cur = p.parent();
            }
            if !skip {
                let s = text.trim();
                if !s.is_empty() {
                    body_parts.push(s.to_string());
                }
            }
        }
    }
    let body = body_parts.join(" ");
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut links = Vec::new();
    for a in dom.select(&a_sel) {
        if let Some(href) = a.value().attr("href") {
            let anchor = a.text().collect::<Vec<_>>().join(" ");
            let anchor = anchor.split_whitespace().collect::<Vec<_>>().join(" ");
            if let Some(abs) = crate::canonical::resolve(page_url, href) {
                links.push((abs, anchor));
            }
        }
    }

    let mut image_alts = Vec::new();
    let mut image_srcs = Vec::new();
    for img in dom.select(&img_sel) {
        if let Some(src) = img.value().attr("src") {
            if image_srcs.len() < 32 {
                if let Some(abs) = crate::canonical::resolve(page_url, src) {
                    image_srcs.push(abs);
                } else if !src.trim().is_empty() {
                    image_srcs.push(src.trim().to_string());
                }
            }
        }
        if let Some(alt) = img.value().attr("alt") {
            let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
            if !alt.is_empty() && image_alts.len() < 32 {
                image_alts.push(alt);
            }
        }
    }

    let canonical = dom
        .select(&canon_sel)
        .filter_map(|e| e.value().attr("href"))
        .next()
        .and_then(|h| crate::canonical::resolve(page_url, h));
    let description = dom
        .select(&meta_sel)
        .filter_map(|e| e.value().attr("content"))
        .next()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty());

    Extracted {
        title,
        headings,
        body,
        links,
        image_alts,
        image_srcs,
        canonical,
        description,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_html_never_panics() {
        // Truncated entities, unclosed tags, nul bytes, huge attrs.
        let cases = [
            "<html><title>unclosed".to_string(),
            "<a href=\"/x\">".repeat(500),
            "<img src=x alt=\"".repeat(200),
            "\u{0}\u{1}\u{2}<p>\u{fffd}".to_string(),
            "<script>".repeat(1000),
            "<table><tr><td>".repeat(300),
            "&#xZZZ;&#99999999;<a href>".to_string(),
        ];
        for html in &cases {
            let e = extract("https://example.com/a", html);
            // Must return *something* sane without panicking.
            let _ = (e.title.len(), e.body.len(), e.links.len());
        }
    }

    const PAGE: &str = r#"<!doctype html><html><head>
        <title>Compilers 101</title>
        <link rel="canonical" href="/compilers">
        <meta name="description" content="An intro to compilers.">
        <style>.x{color:red}</style><script>alert(1)</script>
        </head><body>
        <nav>Home About</nav>
        <h1>Loop Optimization</h1>
        <p>The <b>compiler</b> unrolls loops.</p>
        <a href="/inline">inlining guide</a>
        <img src="/img/loop.png" alt="unrolled loop diagram">
        <img src="https://cdn.example.com/x.jpg">
        <footer>copyright</footer>
        </body></html>"#;

    #[test]
    fn extracts_fields_and_suppresses_boilerplate() {
        let e = extract("https://example.com/a", PAGE);
        assert_eq!(e.title, "Compilers 101");
        assert_eq!(e.headings, vec!["Loop Optimization"]);
        assert!(e.body.contains("compiler unrolls loops"), "{}", e.body);
        assert!(!e.body.contains("alert"), "{}", e.body);
        assert!(!e.body.contains("Home About"), "{}", e.body);
        assert!(!e.body.contains("copyright"), "{}", e.body);
        assert!(!e.body.contains("color"), "{}", e.body);
        assert_eq!(
            e.links,
            vec![(
                "https://example.com/inline".to_string(),
                "inlining guide".to_string()
            )]
        );
        assert_eq!(e.image_alts, vec!["unrolled loop diagram"]);
        assert_eq!(
            e.image_srcs,
            vec![
                "https://example.com/img/loop.png".to_string(),
                "https://cdn.example.com/x.jpg".to_string()
            ]
        );
        assert_eq!(
            e.canonical,
            Some("https://example.com/compilers".to_string())
        );
        assert_eq!(e.description, Some("An intro to compilers.".to_string()));
    }
}
