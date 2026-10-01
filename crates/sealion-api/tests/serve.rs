//! Live API contract test (spec §76–77): search, complete, admin status,
//! metrics, and the admin-token gate, against an ephemeral server.

use std::net::SocketAddr;

use sealion_api::{serve_on, ServerConfig};
use sealion_core::config::Config;

async fn spawn_server(
    data_dir: &std::path::Path,
    admin_token: Option<String>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = ServerConfig {
        data_dir: data_dir.to_path_buf(),
        config: Config::default(),
        admin_token,
    };
    let handle = tokio::spawn(async move {
        let _ = serve_on(listener, cfg).await;
    });
    // Wait for accept.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    (addr, handle)
}

async fn get_json(url: &str) -> serde_json::Value {
    let text = reqwest::get(url).await.unwrap().text().await.unwrap();
    serde_json::from_str(&text).unwrap()
}

fn fixture_index(name: &str) -> std::path::PathBuf {
    use sealion_core::document::{DocId, Document, Source};
    use sealion_index::mem_index::MemIndex;
    use sealion_index::segment::writer::write_segment;
    let dir = std::env::temp_dir().join(format!("sealion-api-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut idx = MemIndex::new(Config::default().analysis.clone());
    idx.add_document(Document {
        id: DocId(1),
        source: Source::Synthetic { name: "api".into() },
        url: "test://compiler".into(),
        title: "Compiler Optimization".into(),
        headings: Vec::new(),
        body: "the compiler unrolls loops".into(),
        anchor_text: Vec::new(),
        metadata: Default::default(),
        timestamp: 0,
        language: "en".into(),
        content_hash: String::new(),
    });
    write_segment(&dir, &idx, true).unwrap();
    dir
}

#[tokio::test]
async fn search_complete_status_metrics() {
    let dir = fixture_index("search");
    let (addr, handle) = spawn_server(&dir, None).await;
    let base = format!("http://{addr}");

    let res = get_json(&format!("{base}/api/search?q=compiler&limit=5")).await;
    assert_eq!(res["query"], "compiler");
    assert_eq!(res["partial"], false);
    assert_eq!(res["results"].as_array().unwrap().len(), 1);
    assert_eq!(res["results"][0]["title"], "Compiler Optimization");
    assert!(res["took_ms"].as_f64().unwrap() >= 0.0);

    let res = get_json(&format!("{base}/api/complete?prefix=comp&limit=5")).await;
    assert!(!res["completions"].as_array().unwrap().is_empty());

    let res = get_json(&format!("{base}/api/admin/status")).await;
    assert_eq!(res["mode"], "single");

    let res = get_json(&format!("{base}/api/admin/metrics")).await;
    assert!(res["searches"].as_u64().unwrap() >= 1);

    // Bad query shape is a 400, not a 500.
    let bad = reqwest::get(format!("{base}/api/search")).await.unwrap();
    assert_eq!(bad.status(), 400u16);

    // Frontend views serve the same single-page bundle (deep links work).
    for path in ["/", "/search?q=compiler", "/stats", "/about"] {
        let page = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(page.status(), 200u16);
        let html = page.text().await.unwrap();
        assert!(html.contains("SeaLion"), "missing brand on {path}");
    }

    // Zero-hit simple query carries a BK-tree correction, never a rewrite.
    let res = get_json(&format!("{base}/api/search?q=compilr&limit=5")).await;
    assert_eq!(res["results"].as_array().unwrap().len(), 0);
    assert_eq!(res["suggestion"], "compil");

    // Healthy queries carry no suggestion.
    let res = get_json(&format!("{base}/api/search?q=compiler&limit=5")).await;
    assert!(res.get("suggestion").is_none());

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn admin_token_gates_privileged_endpoints() {
    let dir = fixture_index("admin");
    let (addr, handle) = spawn_server(&dir, Some("s3cret".into())).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // No token → 401.
    let res = client
        .get(format!("{base}/api/admin/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401u16);
    // Wrong token → 401.
    let res = client
        .get(format!("{base}/api/admin/status"))
        .header("Authorization", "Bearer nope")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401u16);
    // Right token → 200.
    let res = client
        .get(format!("{base}/api/admin/status"))
        .header("Authorization", "Bearer s3cret")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200u16);
    // Public search stays open.
    let res = client
        .get(format!("{base}/api/search?q=compiler"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200u16);

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
