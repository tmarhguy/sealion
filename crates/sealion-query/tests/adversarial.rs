//! Adversarial inputs: fuzz-shaped garbage must error, never panic (§98).
//!
//! Deterministic LCG-generated corpora (in-repo deterministic fuzz; nightly
//! `cargo fuzz` harnesses are future work per `docs/failure-model.md`).
//! Covers: query parser, BK-tree/Levenshtein, edit-distance cap.

use sealion_core::config::AnalysisConfig;
use sealion_query::execute::edit_distance_capped;
use sealion_query::parser::parse;
use sealion_query::spell::{levenshtein, BkTree};

struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

const PIECES: [&str; 25] = [
    "compiler",
    "AND",
    "OR",
    "NOT",
    "\"",
    "(",
    ")",
    ":",
    "title:",
    "site:",
    "*",
    "~",
    "~1",
    "~9",
    " ",
    "  ",
    "\t",
    "a",
    "z*",
    "\"unclosed",
    "(unclosed",
    "Ünïcodé",
    "\u{0}\u{1}",
    "::::",
    "ANDORNOT",
];

#[test]
fn parser_never_panics_on_garbage() {
    let c = AnalysisConfig::default();
    let mut rng = Rng(0xdead_beef);
    for _ in 0..2000 {
        let n = 1 + rng.next(6);
        let q: String = (0..n)
            .map(|_| PIECES[rng.next(PIECES.len())])
            .collect::<Vec<_>>()
            .join("");
        // Must return Ok or InvalidQuery — never panic, never hang.
        let r = parse(&c, 64, &q);
        if let Ok(query) = r {
            assert!(query.leaf_count() <= 64, "limit enforced on {q:?}");
        }
    }
    // Nul bytes, lone surrogates-ish, deep nesting: still total.
    for q in [
        "\u{0}",
        "\"\"\"\"\"",
        "((((((((((a))))))))))",
        "*~:*~:*~",
        "title:",
        "site:",
    ] {
        let _ = parse(&c, 64, q);
    }
}

#[test]
fn levenshtein_is_a_metric_on_garbage() {
    let mut rng = Rng(0xcafe_f00d);
    let words: Vec<String> = (0..60)
        .map(|_| {
            (0..1 + rng.next(8))
                .map(|_| (b'a' + rng.next(26) as u8) as char)
                .collect()
        })
        .collect();
    for _ in 0..500 {
        let (a, b, c) = (
            &words[rng.next(60)],
            &words[rng.next(60)],
            &words[rng.next(60)],
        );
        let (ab, bc, ac) = (levenshtein(a, b), levenshtein(b, c), levenshtein(a, c));
        assert_eq!(levenshtein(a, a), 0);
        assert_eq!(ab, levenshtein(b, a), "symmetric");
        assert!(ac <= ab + bc, "triangle inequality");
    }
}

#[test]
fn bk_tree_survives_garbage_vocab() {
    let mut rng = Rng(0x1234_abcd);
    let vocab: Vec<String> = (0..200)
        .map(|_| {
            (0..1 + rng.next(10))
                .map(|_| (b'!' + rng.next(90) as u8) as char)
                .collect()
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let tree = BkTree::build(vocab.iter().map(String::as_str));
    for _ in 0..100 {
        let probe: String = (0..1 + rng.next(10))
            .map(|_| (b'!' + rng.next(90) as u8) as char)
            .collect();
        let mut got: Vec<(String, usize)> = tree.query(&probe, 2);
        got.sort();
        let mut want: Vec<(String, usize)> = vocab
            .iter()
            .filter_map(|v| {
                let d = levenshtein(v, &probe);
                (d <= 2).then(|| (v.clone(), d))
            })
            .collect();
        want.sort();
        assert_eq!(got, want, "bk-tree == scan on {probe:?}");
    }
}

#[test]
fn capped_distance_agrees_with_exact_within_cap() {
    let mut rng = Rng(0x777);
    for _ in 0..500 {
        let mut mk = || {
            (0..1 + rng.next(8))
                .map(|_| (b'a' + rng.next(4) as u8) as char)
                .collect::<String>()
        };
        let (a, b) = (mk(), mk());
        let exact = levenshtein(&a, &b);
        for cap in [1u8, 2] {
            let got = edit_distance_capped(&a, &b, cap);
            if exact <= cap as usize {
                assert_eq!(got as usize, exact, "{a:?} vs {b:?}");
            } else {
                assert!(got > cap, "{a:?} vs {b:?}");
            }
        }
    }
}
