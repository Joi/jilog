//! Spool writer -- producer side.
//!
//! Used by producer machines (every host in the fleet) to write sealed
//! segments into the spool for later replication and ingestion by the
//! configured authority host (in this fleet, jibotmac).
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

    /// Write a sealed segment to the spool (no-clobber).
    ///
    /// The segment must be sealed (checksum != 0) before writing, and
    /// its `source` must satisfy [`crate::valid_source_name`] — the
    /// filename (and therefore the destination path) is derived from
    /// the source, so this check is what keeps a hostile or corrupt
    /// `source` field from escaping `incoming/` via path traversal.
    /// Callers validate too; this is defense in depth.
    ///
    /// Publication uses [`Segment::publish_new`] (fsynced unique tmp +
    /// `hard_link`, which FAILS instead of replacing): an existing file
    /// at the destination — including one that appears in the
    /// check-to-publish window, real in a Syncthing-synced spool — is
    /// never overwritten. Identical existing content is an idempotent
    /// skip; different content is an error with both files left intact.
    /// Returns the path where the segment lives together with the
    /// publish outcome, so callers can count an `AlreadyIdentical` as
    /// skipped rather than freshly written.
    pub fn write(
        &self,
        segment: &Segment,
    ) -> Result<(PathBuf, ledger_core::PublishOutcome), SpoolError> {
        if !crate::valid_source_name(&segment.source) {
            return Err(SpoolError::InvalidSource {
                name: segment.source.clone(),
            });
        }

        self.ensure_dirs()?;

        let path = self.incoming_dir.join(segment.filename());

        let outcome = segment.publish_new(&path)?;
        match &outcome {
            ledger_core::PublishOutcome::Published => {
                tracing::info!(
                    source = %segment.source,
                    seq = segment.source_seq,
                    path = %path.display(),
                    "segment written to spool"
                );
            }
            ledger_core::PublishOutcome::AlreadyIdentical => {
                tracing::debug!(
                    path = %path.display(),
                    "identical segment already in spool, skipping"
                );
            }
        }

        Ok((path, outcome))
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

        let (path, outcome) = writer.write(&seg).unwrap();
        assert_eq!(outcome, ledger_core::PublishOutcome::Published);
        assert!(path.exists());
        assert!(path.ends_with("jibotmac-000001.json"));

        // Read it back to verify.
        let loaded = Segment::read_from_file(&path).unwrap();
        assert_eq!(loaded.source, "jibotmac");
        assert!(loaded.verify().unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_rejects_traversal_source() {
        let dir = std::env::temp_dir().join("opsctl-test-spool-writer-traversal");
        let _ = std::fs::remove_dir_all(&dir);

        let writer = SpoolWriter::new(&dir);
        for bad in ["../../evil", "/etc/cron.d/x", "a/b", ".hidden"] {
            let mut seg = Segment::new(bad, 1);
            seg.append(test_event());
            seg.seal().unwrap();
            let err = writer.write(&seg).unwrap_err();
            assert!(
                matches!(err, crate::SpoolError::InvalidSource { .. }),
                "{bad:?}: expected InvalidSource, got {err}"
            );
        }
        // Nothing may have been written anywhere under (or outside) dir.
        assert!(
            !dir.join("incoming").exists(),
            "rejected write must not create files"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_conflicting_existing_is_error_and_preserves_both() {
        let dir = std::env::temp_dir().join("opsctl-test-spool-writer-conflict");
        let _ = std::fs::remove_dir_all(&dir);

        let writer = SpoolWriter::new(&dir);
        let mut first = Segment::new("jibotmac", 1);
        first.append(test_event());
        first.seal().unwrap();
        writer.write(&first).unwrap();

        // Same identity, different content: must NOT clobber.
        let mut second = Segment::new("jibotmac", 1);
        second.append(test_event());
        second.append(test_event());
        second.seal().unwrap();
        let err = writer.write(&second).unwrap_err();
        assert!(
            err.to_string().contains("DIFFERENT content"),
            "unexpected error: {err}"
        );

        // The original file is intact, and no tmp junk remains.
        let on_disk =
            Segment::read_from_file(dir.join("incoming/jibotmac-000001.json")).unwrap();
        assert!(on_disk.content_matches(&first), "existing file must be preserved");
        assert_eq!(
            std::fs::read_dir(dir.join("incoming")).unwrap().count(),
            1,
            "no extra files after the conflict"
        );

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
        let (path2, outcome) = writer.write(&seg).unwrap(); // second write
        assert_eq!(
            outcome,
            ledger_core::PublishOutcome::AlreadyIdentical,
            "second identical write must report a skip, not a fresh write"
        );
        assert!(path2.exists()); // no error, just skips

        let _ = std::fs::remove_dir_all(&dir);
    }
}
