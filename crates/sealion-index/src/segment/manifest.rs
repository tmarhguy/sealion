//! Segment manifest (spec §19): the atomic publication record.
//!
//! The manifest is the only mutable file in the data directory: an ordered
//! list of live segment files plus a generation counter. Updates are
//! write-temp + `fsync` + atomic rename, so readers never observe a
//! partially written generation. Segment files themselves are immutable
//! and content-complete before they are named here.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use sealion_core::error::{Error, Result};
use serde::{Deserialize, Serialize};

use super::MANIFEST_NAME;

/// One published index generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Monotonic generation; bumped on every publication.
    pub generation: u64,
    /// Live segment files in publication order.
    pub segments: Vec<String>,
}

impl Manifest {
    pub fn new() -> Self {
        Self { generation: 0, segments: Vec::new() }
    }

    /// Next segment id (one higher than any published so far).
    pub fn next_id(&self) -> u64 {
        self.generation + 1
    }

    /// Load the manifest, or an empty one if none is published yet.
    /// A corrupt manifest is an error, never silently ignored.
    pub fn load_or_new(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_NAME);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Corrupt(format!("manifest {MANIFEST_NAME}: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(Error::Io(e.to_string())),
        }
    }

    /// Register a newly published segment file.
    pub fn add_segment(&mut self, file_name: String) {
        if !self.segments.contains(&file_name) {
            self.segments.push(file_name);
        }
        self.generation += 1;
    }

    /// Remove a segment file from the generation (merge cleanup, §23).
    pub fn remove_segment(&mut self, file_name: &str) {
        self.segments.retain(|s| s != file_name);
        self.generation += 1;
    }

    /// Atomically publish this generation.
    pub fn store(&self, dir: &Path) -> Result<()> {
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|e| Error::Internal(e.to_string()))?;
        let tmp = dir.join(format!("{MANIFEST_NAME}.tmp"));
        {
            let mut f = File::create(&tmp).map_err(|e| Error::Io(e.to_string()))?;
            f.write_all(&bytes).map_err(|e| Error::Io(e.to_string()))?;
            f.sync_all().map_err(|e| Error::Io(e.to_string()))?;
        }
        std::fs::rename(&tmp, dir.join(MANIFEST_NAME)).map_err(|e| Error::Io(e.to_string()))?;
        let d = File::open(dir).map_err(|e| Error::Io(e.to_string()))?;
        d.sync_all().map_err(|e| Error::Io(e.to_string()))?;
        Ok(())
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sealion-manifest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn publish_round_trip_is_atomic() {
        let dir = tmpdir("roundtrip");
        let loaded = Manifest::load_or_new(&dir).unwrap();
        assert_eq!(loaded.generation, 0);
        assert!(loaded.segments.is_empty());

        let mut m = loaded;
        m.add_segment("seg-000001.seal".into());
        m.store(&dir).unwrap();
        // No temp file may survive publication.
        assert!(!dir.join(format!("{MANIFEST_NAME}.tmp")).exists());

        let back = Manifest::load_or_new(&dir).unwrap();
        assert_eq!(back.generation, 1);
        assert_eq!(back.segments, vec!["seg-000001.seal".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_manifest_is_an_error() {
        let dir = tmpdir("corrupt");
        std::fs::write(dir.join(MANIFEST_NAME), b"{not json").unwrap();
        assert!(matches!(Manifest::load_or_new(&dir), Err(Error::Corrupt(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
