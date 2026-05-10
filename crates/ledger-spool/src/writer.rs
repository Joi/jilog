//! Spool writer -- producer side.
//!
//! Used by remote machines (jibotmac) to write sealed segments into
//! the spool for later replication and ingestion by the authority.
//!
//! # Rust concepts in this file
//!
//! - **Simple struct + methods**: SpoolWriter is just a directory path
//!   with methods to write segments. No traits, no generics -- just
//!   the simplest thing that works. You can always add abstraction later.

use std::fs;
use std::path::{Path, PathBuf};

use ledger_core::Segment;

use crate::error::SpoolError;

/// Writes segments to the spool's incoming directory.
pub struct SpoolWriter {
    /// Path to the spool's incoming/ directory.
    incoming_dir: PathBuf,
}

impl SpoolWriter {
    /// Create a new spool writer for the given spool root.
    ///
    /// The spool root is the zone-level spool directory.
    /// Segments are written to `{spool_root}/incoming/`.
    pub fn new(spool_root: impl AsRef<Path>) -> Self {
        Self {
            incoming_dir: spool_root.as_ref().join("incoming"),
        }
    }

    /// Ensure the spool directories exist.
    pub fn ensure_dirs(&self) -> Result<(), SpoolError> {
        fs::create_dir_all(&self.incoming_dir)?;
        Ok(())
    }

    /// Write a sealed segment to the spool.
    ///
    /// The segment must be sealed (checksum != 0) before writing.
    /// Returns the path where the segment was written.
    pub fn write(&self, segment: &Segment) -> Result<PathBuf, SpoolError> {
        self.ensure_dirs()?;

        let path = self.incoming_dir.join(segment.filename());

        // Don't overwrite existing files (idempotent producers should
        // check before writing).
        if path.exists() {
            tracing::debug!(
                path = %path.display(),
                "segment already in spool, skipping"
            );
            return Ok(path);
        }

        segment.write_to_file(&path)?;

        tracing::info!(
            source = %segment.source,
            seq = segment.source_seq,
            path = %path.display(),
            "segment written to spool"
        );

        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledger_core::{Event, EventClass, PayloadTier, Segment};
    use chrono::Utc;
    use uuid::Uuid;

    fn test_event() -> Event {
        Event {
            event_id: Uuid::now_v7(),
            zone: "test".to_string(),
            source: "jibotmac".to_string(),
            source_seq: 1,
            timestamp: Utc::now(),
            correlation_id: None,
            causation_id: None,
            actor_ref: None,
            object_ref: None,
            event_class: EventClass::Health,
            payload_tier: PayloadTier::MetadataOnly,
            payload: None,
        }
    }

    #[test]
    fn test_write_to_spool() {
        let dir = std::env::temp_dir().join("opsctl-test-spool-writer");
        let _ = std::fs::remove_dir_all(&dir);

        let writer = SpoolWriter::new(&dir);

        let mut seg = Segment::new("jibotmac", 1);
        seg.append(test_event());
        seg.seal().unwrap();

        let path = writer.write(&seg).unwrap();
        assert!(path.exists());
        assert!(path.ends_with("jibotmac-000001.json"));

        // Read it back to verify.
        let loaded = Segment::read_from_file(&path).unwrap();
        assert_eq!(loaded.source, "jibotmac");
        assert!(loaded.verify().unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_is_idempotent() {
        let dir = std::env::temp_dir().join("opsctl-test-spool-writer-idem");
        let _ = std::fs::remove_dir_all(&dir);

        let writer = SpoolWriter::new(&dir);

        let mut seg = Segment::new("jibotmac", 1);
        seg.append(test_event());
        seg.seal().unwrap();

        writer.write(&seg).unwrap();
        let path2 = writer.write(&seg).unwrap(); // second write
        assert!(path2.exists()); // no error, just skips

        let _ = std::fs::remove_dir_all(&dir);
    }
}
