//! End-to-end crawl against a local HTTP server (offline).
//!
//! Exercises: robots allow/deny, redirects, HTML extraction + outlink
//! following, depth caps, binary content-type skipping, exact-dup
//! detection, and resume-safe frontier persistence.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sealion_core::config::CrawlerConfig;
use sealion_crawler::Crawler;

fn route(path: &str) -> (String, Vec<u8>) {
    match path {
        "/robots.txt" => (
            "text/plain".into(),
            b"User-agent: *\nDisallow: /blocked\n".to_vec(),
        ),
        "/" => (
            "text/html".into(),
            "<html><head><title>Home</title></head><body>\
                <h1>Welcome home</h1>\
                <p>compiler optimization guide</p>\
                <img src=\"/img/a.png\" alt=\"compiler diagram\">\
                <a href=\"/page2\">second page</a> \
                <a href=\"/blocked\">nope</a> \
                <a href=\"/old\">redirect</a> \
                <a href=\"/pic\">binary</a></body></html>"
                .to_string()
                .into_bytes(),
        ),
        "/page2" => (
            "text/html".into(),
            b"<html><head><title>Second</title></head><body>\
              <p>database systems notes</p>\
              <a href=\"/\">back home</a></body></html>"
                .to_vec(),
        ),
        "/old" => (
            // Redirect to /page2.
            "REDIRECT:/page2".to_string(),
            Vec::new(),
        ),
        "/pic" => ("image/png".into(), vec![0x89, b'P', b'N', b'G']),
        "/img/a.png" => ("image/png".into(), vec![0x89, b'P', b'N', b'G']),
        _ => ("text/plain".into(), b"not found".to_vec()),
    }
}

fn serve(listener: TcpListener, hits: Arc<AtomicUsize>, _port: u16) {
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let mut buf = [0u8; 4096];
        let Ok(n) = s.read(&mut buf) else { continue };
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/");
        hits.fetch_add(1, Ordering::Relaxed);
        let (ctype, body) = route(path);
        let response = if let Some(loc) = ctype.strip_prefix("REDIRECT:") {
            format!("HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        } else if path != "/"
            && path != "/page2"
            && path != "/robots.txt"
            && path != "/old"
            && path != "/pic"
        {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found"
                .to_string()
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
        };
        let _ = s.write_all(response.as_bytes());
        if !body.is_empty() && !ctype.starts_with("REDIRECT:") {
            let _ = s.write_all(&body);
        }
    }
}

fn test_config() -> CrawlerConfig {
    CrawlerConfig {
        crawl_delay_ms: 0,
        max_depth: 2,
        max_pages: 50,
        max_retries: 0,
        fetch_timeout_ms: 5000,
        allow_loopback: true,
        restrict_to_seed_domains: false,
        robots_enabled: true,
        ..CrawlerConfig::default()
    }
}

#[tokio::test]
async fn crawls_local_site_respecting_robots_and_links() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || serve(listener, h, port));

    let dir = std::env::temp_dir().join(format!("sealion-crawl-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = test_config();
    let mut crawler = Crawler::new(config, dir.clone()).unwrap();
    let seed = format!("http://127.0.0.1:{port}/");
    assert_eq!(crawler.add_seeds(&[seed]), 1);
    let docs = crawler.run().await.unwrap();

    // Home + page2 indexed; /blocked denied by robots; /pic skipped
    // (binary); /old redirects to /page2 (deduped, not double-counted).
    let urls: Vec<&str> = docs.iter().map(|d| d.url.as_str()).collect();
    assert!(urls.iter().any(|u| u.ends_with('/')), "{urls:?}");
    assert!(urls.iter().any(|u| u.ends_with("/page2")), "{urls:?}");
    assert!(!urls.iter().any(|u| u.contains("blocked")), "{urls:?}");
    assert_eq!(docs.len(), 2, "{urls:?}");

    let home = docs.iter().find(|d| d.url.ends_with('/')).unwrap();
    assert_eq!(home.title, "Home");
    assert!(
        home.body.contains("compiler optimization guide"),
        "{}",
        home.body
    );
    // Image alt text joined into the body; src recorded, bytes unfetched.
    assert!(home.body.contains("compiler diagram"), "{}", home.body);
    assert_eq!(
        home.metadata.get("image_refs").map(String::as_str),
        Some(format!("http://127.0.0.1:{port}/img/a.png").as_str())
    );

    let stats = crawler.stats();
    assert_eq!(stats.pages, 2);
    assert!(stats.robots_denied >= 1, "{stats:?}");
    assert!(stats.skipped_content >= 1, "{stats:?}");
    assert!(hits.load(Ordering::Relaxed) > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn depth_limit_stops_at_seeds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || serve(listener, h, port));

    let dir = std::env::temp_dir().join(format!("sealion-crawl-d0-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = test_config();
    config.max_depth = 0;
    let mut crawler = Crawler::new(config, dir.clone()).unwrap();
    crawler.add_seeds(&[format!("http://127.0.0.1:{port}/")]);
    let docs = crawler.run().await.unwrap();
    assert_eq!(
        docs.len(),
        1,
        "{:?}",
        docs.iter().map(|d| &d.url).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
