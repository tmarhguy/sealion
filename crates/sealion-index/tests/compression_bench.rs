//! Postings codec micro-benchmarks (spec §20, §89).
//!
//! Compares raw u64 storage against delta+varint on synthetic posting
//! shapes, reporting bytes/posting and decode throughput. Throughput
//! numbers are printed (machine-dependent; run with `-- --nocapture`) and
//! recorded in `docs/adr/005-postings-codec.md`; assertions cover only
//! correctness and the size improvement that must hold on realistic data.
//! Segment-level compression is measured in
//! `sealion-query/tests/compression_segments.rs` (it needs both crates).

use std::time::Instant;

use sealion_index::codec::{decode_deltas, encode_deltas};

/// Deterministic LCG (no dev-dependencies for benchmarking).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Posting shapes: dense run, sparse uniform, clustered runs.
fn shapes() -> Vec<(&'static str, Vec<u64>)> {
    let dense: Vec<u64> = (1..=100_000).collect();
    let mut rng = Rng(0xC0FFEE);
    let mut sparse: Vec<u64> = (0..20_000).map(|_| rng.next() % 5_000_000).collect();
    sparse.sort();
    sparse.dedup();
    let mut clustered = Vec::new();
    let mut base = 0;
    for _ in 0..400 {
        let run = 1 + rng.next() % 300;
        clustered.extend(base..base + run);
        base += run + rng.next() % 50_000;
    }
    vec![
        ("dense-100k", dense),
        ("sparse-20k/5M", sparse),
        ("clustered", clustered),
    ]
}

fn raw_size(ids: &[u64]) -> usize {
    8 + ids.len() * 8
}

fn bench_decode(ids: &[u64], buf: &[u8]) -> (f64, f64) {
    // Returns (MB/s, postings/s) using the best of several runs.
    let mut best = f64::INFINITY;
    for _ in 0..10 {
        let t = Instant::now();
        let (back, used) = decode_deltas(buf).unwrap();
        std::hint::black_box(back);
        let dt = t.elapsed().as_secs_f64();
        assert_eq!(used, buf.len());
        if dt < best {
            best = dt;
        }
    }
    let secs = best.max(1e-9);
    (buf.len() as f64 / 1e6 / secs, ids.len() as f64 / secs)
}

#[test]
fn compression_report() {
    println!(
        "\n{:<16} {:>10} {:>10} {:>8} {:>12} {:>14}",
        "shape", "n", "raw-B", "comp-B", "B/posting", "decode"
    );
    for (name, ids) in shapes() {
        let mut buf = Vec::new();
        encode_deltas(&ids, &mut buf);
        let (mbs, per_s) = bench_decode(&ids, &buf);
        println!(
            "{:<16} {:>10} {:>10} {:>8} {:>12.2} {:>7.1} MB/s {:>10.0} post/s",
            name,
            ids.len(),
            raw_size(&ids),
            buf.len(),
            buf.len() as f64 / ids.len() as f64,
            mbs,
            per_s
        );
        // Correctness: exact round-trip on every shape.
        assert_eq!(decode_deltas(&buf).unwrap().0, ids);
        // Size: delta+varint must beat raw u64s on realistic shapes.
        assert!(buf.len() < raw_size(&ids), "{name}: no compression benefit");
    }
}
