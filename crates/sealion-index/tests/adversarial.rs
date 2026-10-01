//! Adversarial segment/codec/manifest inputs (§98–99).
//!
//! Garbage must produce `Err`, never panic or silent corruption. Region
//! sampling corrupts every part of a real segment file (header, blocks,
//! dictionary, metadata, stats, footer) and requires detection.

use sealion_core::config::AnalysisConfig;
use sealion_core::document::{DocId, Document, Source};
use sealion_index::codec::{decode_deltas, encode_deltas};
use sealion_index::mem_index::MemIndex;
use sealion_index::segment::manifest::Manifest;
use sealion_index::segment::reader::SegmentReader;
use sealion_index::segment::writer::write_segment;

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

fn doc(id: u64, body: &str) -> Document {
    Document {
        id: DocId(id),
        source: Source::Synthetic { name: "adv".into() },
        url: format!("test://{id}"),
        title: format!("t{id}"),
        headings: Vec::new(),
        body: body.into(),
        anchor_text: Vec::new(),
        metadata: Default::default(),
        timestamp: 0,
        language: "en".into(),
        content_hash: String::new(),
    }
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sealion-adv-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_segment() -> (std::path::PathBuf, String) {
    let dir = tmpdir("seg");
    let mut idx = MemIndex::new(AnalysisConfig::default());
    idx.add_document(doc(1, "alpha beta gamma delta epsilon"));
    idx.add_document(doc(2, "beta gamma common words everywhere"));
    idx.add_document(doc(3, "rare zephyrquux term here"));
    let meta = write_segment(&dir, &idx, true).unwrap();
    (dir, meta.file_name)
}

#[test]
fn garbage_bytes_never_open() {
    let dir = tmpdir("garbage");
    let mut rng = Rng(0xbad_c0de);
    for i in 0..200 {
        let len = rng.next(300);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next(256) as u8).collect();
        let path = dir.join(format!("g{i}.seal"));
        std::fs::write(&path, &bytes).unwrap();
        // Must be Err (or, for empty reads, still Err) — never panic.
        assert!(SegmentReader::open(&path).is_err(), "garbage {i} opened!");
    }
    // Truncations of a valid file also fail.
    let (sdir, name) = sample_segment();
    let bytes = std::fs::read(sdir.join(&name)).unwrap();
    for cut in [0, 1, 10, 59, 60, 61, bytes.len() / 2, bytes.len() - 1] {
        let path = dir.join(format!("t{cut}.seal"));
        std::fs::write(&path, &bytes[..cut.min(bytes.len())]).unwrap();
        assert!(
            SegmentReader::open(&path).is_err(),
            "truncation {cut} opened!"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&sdir);
}

#[test]
fn corruption_detected_in_every_region() {
    let (dir, name) = sample_segment();
    let bytes = std::fs::read(dir.join(&name)).unwrap();
    let mut rng = Rng(0x9e6109);
    let mut checked = 0;
    // Sample 40 offsets across the whole file: any single-byte change must
    // be caught by header/file/block CRCs or structural validation.
    for _ in 0..40 {
        let off = rng.next(bytes.len());
        let mut bad = bytes.clone();
        bad[off] ^= 0xFF;
        let path = dir.join("mut.seal");
        std::fs::write(&path, &bad).unwrap();
        assert!(
            SegmentReader::open(&path).is_err(),
            "mutation at offset {off} undetected!"
        );
        checked += 1;
    }
    assert!(checked > 0);
    // The pristine file still opens (mutations didn't touch it).
    assert!(SegmentReader::open(&dir.join(&name)).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn delta_codec_rejects_garbage() {
    let mut rng = Rng(0xc0dec);
    for _ in 0..500 {
        let len = rng.next(24);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next(256) as u8).collect();
        // Must return None or a self-consistent decode — never panic, never
        // overrun the input.
        if let Some((vals, used)) = decode_deltas(&bytes) {
            assert!(used <= bytes.len());
            // Re-encoding the decoded values must decode back identically.
            let mut re = Vec::new();
            encode_deltas(&vals, &mut re);
            let back = decode_deltas(&re).expect("re-encode must decode");
            assert_eq!(back.0, vals);
        }
    }
    // Empty input decodes to empty (length-prefixed zero), not panic.
    assert_eq!(decode_deltas(&[]), None);
}

#[test]
fn manifest_garbage_is_an_error() {
    let dir = tmpdir("manifest");
    for (i, content) in [
        "not json at all",
        "{\"generation\": \"x\"}",
        "[1,2,3]",
        "{\"generation\": 1, \"segments\": \"nope\"}",
        "",
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(dir.join("manifest.json"), content).unwrap();
        assert!(
            Manifest::load_or_new(&dir).is_err(),
            "manifest {i} accepted!"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
