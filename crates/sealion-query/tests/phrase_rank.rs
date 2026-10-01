//! Phrase agreement + BM25 sanity (milestone 07).
//!
//! - Indexed phrase search equals the positional reference oracle.
//! - Ranked search returns score-desc order with explain totals matching.

use sealion_core::config::{AnalysisConfig, RankingConfig, StemmerKind};
use sealion_core::document::{DocId, Document, Source};
use sealion_core::field::Field;
use sealion_index::analysis::Analyzer;
use sealion_index::mem_index::MemIndex;
use sealion_query::execute::search_view;
use sealion_query::query::Query;
use sealion_query::rank::ranked_search;
use sealion_query::reference::reference_phrase;

fn cfg() -> AnalysisConfig {
    AnalysisConfig {
        stop_words: false,
        stemmer: StemmerKind::None,
        ..AnalysisConfig::default()
    }
}

fn doc(id: u64, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic { name: "p".into() },
        url: format!("t://{id}"),
        title: String::new(),
        headings: Vec::new(),
        body: body.into(),
        anchor_text: Vec::new(),
        metadata: Default::default(),
        timestamp: 0,
        language: "en".into(),
        content_hash: String::new(),
    }
}

#[test]
fn phrase_index_matches_positional_oracle() {
    let a = cfg();
    let docs = vec![
        doc(1, "distributed database systems"),
        doc(2, "database distributed systems"),
        doc(3, "distributed consensus and database systems"),
    ];
    let mut idx = MemIndex::new(a.clone());
    for d in &docs {
        idx.add_document(d.clone());
    }
    let analyzer = Analyzer::new(&a);
    for raw in [
        "distributed database",
        "database systems",
        "distributed systems",
    ] {
        let q = Query::phrase_raw(&analyzer, Field::Body, raw);
        let got = search_view(&idx, &q).unwrap();
        let terms: Vec<String> = match &q {
            Query::Phrase { terms, .. } => terms.clone(),
            Query::Term { term, .. } => vec![term.clone()],
            _ => panic!("expected phrase/term"),
        };
        let want = reference_phrase(&docs, &a, Field::Body, &terms);
        assert_eq!(got, want, "phrase {raw:?}");
    }
}

#[test]
fn phrase_hits_truly_contain_the_phrase() {
    // Oracle-independent positional invariant (§96) on randomized content:
    // bigrams taken from real docs must hit their source, and every hit
    // must really contain the adjacency.
    use sealion_index::mem_index::MemIndex;
    use sealion_query::execute::phrase_docs;
    let a = cfg();
    let vocab = ["alpha", "beta", "gamma", "delta", "common", "rare"];
    let mut rng: u64 = 0x1234_5678;
    let mut next = move || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (rng >> 33) as usize
    };
    let mut idx = MemIndex::new(a);
    for id in 0..30u64 {
        let body = (0..8)
            .map(|_| vocab[next() % vocab.len()])
            .collect::<Vec<_>>()
            .join(" ");
        idx.add_document(doc(id, &body));
    }
    let mut checked = 0;
    for id in idx.all_doc_ids() {
        let words: Vec<String> = idx
            .get(id)
            .unwrap()
            .body
            .split_whitespace()
            .map(str::to_string)
            .collect();
        for w in words.windows(2) {
            let terms = vec![w[0].clone(), w[1].clone()];
            let hits = phrase_docs(&idx, Field::Body, &terms).unwrap();
            assert!(hits.contains(&id), "phrase {terms:?} must hit doc {}", id.0);
            for h in &hits {
                let ws: Vec<&str> = idx.get(*h).unwrap().body.split_whitespace().collect();
                assert!(
                    ws.windows(2)
                        .any(|x| x == [terms[0].as_str(), terms[1].as_str()]),
                    "phrase {terms:?} falsely hit doc {}",
                    h.0
                );
            }
            checked += 1;
            if checked >= 60 {
                return;
            }
        }
    }
    assert!(checked > 0);
}

#[test]
fn ranked_search_is_score_ordered_with_matching_totals() {
    let a = cfg();
    let r = RankingConfig::default();
    let mut idx = MemIndex::new(a.clone());
    idx.add_document(doc(1, "compiler compiler compiler"));
    idx.add_document(doc(2, "compiler"));
    idx.add_document(doc(3, "nothing relevant"));
    let q = Query::term(Some(Field::Body), "compiler");
    let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits[0].score >= hits[1].score);
    for h in &hits {
        let recomputed: f32 = h.explain.terms.iter().map(|t| t.contrib).sum();
        assert!((recomputed - h.score).abs() < 1e-4, "{h:?}");
        assert!((recomputed - h.explain.total).abs() < 1e-4);
    }
}
