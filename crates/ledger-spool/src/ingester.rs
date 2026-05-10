//! Spool ingester -- consumer side (authority machine).
//!
//! Scans the spool's `incoming/` directory for new segments,
//! validates them, commits to the authoritative SegmentStore,
//! and moves processed files to `processed/`.
//!
//! # Rust concepts in this file
//!
//! - **Best-effort batch processing**: Unlike the claim flows (which
//!   bail on first error), the ingester processes ALL segments and
//!   collects successes and failures separately. This is the right
//!   pattern for batch work -- one corrupted segment shouldn't prevent
//!   ingesting the other 99 good ones.
//!
//! - **`std::fs::rename`**: Atomic move on the same filesystem. After
//!   a segment is committed, we move it from incoming/ to processed/
//!   instead of deleting it. This creates an audit trail and makes
//!   recovery easier if something goes wrong.
//!
//! - **Return struct instead of tuple**: `IngestReport` is clearer
//!   than `(Vec<String>, Vec<(String, String)>)`. Named fields are
//!   self-documenting. In Python you'd use a dataclass; in Rust,
//!   a plain struct.

use std::fs;
use std::path::{Path, PathBuf};

use ledger_core::{Segment, SegmentStore};

use crate::error::SpoolError;

/// Result of an ingest operation.
#[derive(Debug)]
pub struct IngestReport {
    /// Segments successfully committed.
    pub committed: Vec<String>,

    /// Segments skipped (already in store -- duplicate).
    pub skipped: Vec<String>,

    /// Segments that failed (filename, error message).
    pub failed: Vec<(String, String)>,
}

impl IngestReport {
    fn new() -> Self {
        Self {
            committed: Vec::new(),
            skipped: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Total segments processed (committed + skipped + failed).
    pub fn total(&self) -> usize {
        self.committed.len() + self.skipped.len() + self.failed.len()
    }

    /// Print a human-readable summary.
    pub fn print_summary(&self) {
        println!(
            "Spool ingest: {} committed, {} skipped, {} failed ({} total)",
            self.committed.len(),
            self.skipped.len(),
            self.failed.len(),
            self.total()
        );

        if !self.failed.is_empty() {
            println!("\nFailures:");
            for (file, err) in &self.failed {
                println!("  {} -- {}", file, err);
            }
        }
    }
}

/// Ingests segments from a spool into the authoritative ledger store.
pub struct SpoolIngester {
    /// Path to the spool root (contains incoming/ and processed/).
    spool_root: PathBuf,
}

impl SpoolIngester {
    pub fn new(spool_root: impl AsRef<Path>) -> Self {
        Self {
            spool_root: spool_root.as_ref().to_path_buf(),
        }
    }

    fn incoming_dir(&self) -> PathBuf {
        self.spool_root.join("incoming")
    }

    fn processed_dir(&self) -> PathBuf {
        self.spool_root.join("processed")
    }

    /// Ingest all pending segments from the spool into the store.
    ///
    /// For each segment in `incoming/`:
    /// 1. Read and parse the segment file
    /// 2. Verify the checksum
    /// 3. Attempt to write to the authoritative store
    /// 4. On success: move to `processed/`
    /// 5. On duplicate: move to `processed/` (idempotent)
    /// 6. On failure: leave in `incoming/` and record the error
    pub fn ingest(&self, store: &SegmentStore) -> Result<IngestReport, SpoolError> {
        let incoming = self.incoming_dir();
        let processed = self.processed_dir();

        if !incoming.exists() {
            // No spool directory = nothing to ingest.
            return Ok(IngestReport::new());
        }

        fs::create_dir_all(&processed)?;

        let mut report = IngestReport::new();

        // Collect and sort files for deterministic processing order.
        let mut files: Vec<PathBuf> = fs::read_dir(&incoming)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension()?.to_str()? == "json" {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        files.sort();

        for path in &files {
            let filename = path.file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("unknown")
                .to_string();

            match self.ingest_one(path, store) {
                Ok(IngestOutcome::Committed) => {
                    // Move to processed.
                    let dest = processed.join(&filename);
                    if let Err(e) = fs::rename(path, &dest) {
                        tracing::warn!(
                            file = %filename,
                            error = %e,
                            "failed to move committed segment to processed/"
                        );
                    }
                    report.committed.push(filename);
                }
                Ok(IngestOutcome::Duplicate) => {
                    // Also move to processed (it's already in the store).
                    let dest = processed.join(&filename);
                    let _ = fs::rename(path, &dest);
                    report.skipped.push(filename);
                }
                Err(e) => {
                    // Leave in incoming for retry.
                    tracing::warn!(file = %filename, error = %e, "spool ingest failed");
                    report.failed.push((filename, e.to_string()));
                }
            }
        }

        tracing::info!(
            committed = report.committed.len(),
            skipped = report.skipped.len(),
            failed = report.failed.len(),
            "spool ingest complete"
        );

        Ok(report)
    }

    /// Ingest a single segment file.
    fn ingest_one(
        &self,
        path: &Path,
        store: &SegmentStore,
    ) -> Result<IngestOutcome, SpoolError> {
        // Read the segment.
        let segment = Segment::read_from_file(path)?;

        // Verify integrity.
        let valid = segment.verify()?;
        if !valid {
            return Err(SpoolError::IntegrityFailure {
                src: segment.source.clone(),
                seq: segment.source_seq,
                reason: "checksum mismatch".to_string(),
            });
        }

        // Try to write to the authoritative store.
        match store.write_segment(&segment) {
            Ok(()) => Ok(IngestOutcome::Committed),
            Err(ledger_core::LedgerError::DuplicateSegment { .. }) => {
                Ok(IngestOutcome::Duplicate)
            }
            Err(e) => Err(SpoolError::Ledger(e)),
        }
    }
}

/// Outcome of ingesting a single segment.
enum IngestOutcome {
    /// New segment committed to the store.
    Committed,
    /// Segment already exists in the store (duplicate, harmless).
    Duplicate,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::SpoolWriter;
    use ledger_core::{Event, EventClass, PayloadTier, Segment, SegmentStore, ZoneId};
    use chrono::Utc;
    use uuid::Uuid;

    fn test_event(source: &str, seq: u64) -> Event {
        Event {
            event_id: Uuid::now_v7(),
            zone: "test".to_string(),
            source: source.to_string(),
            source_seq: seq,
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

    fn sealed_segment(source: &str, seq: u64) -> Segment {
        let mut seg = Segment::new(source, seq);
        seg.append(test_event(source, seq));
        seg.seal().unwrap();
        seg
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("opsctl-test-spool").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn test_ingest_from_spool() {
        let dir = test_dir("ingest-basic");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        // Producer writes to spool.
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();
        writer.write(&sealed_segment("jibotmac", 2)).unwrap();

        // Authority ingests from spool.
        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let ingester = SpoolIngester::new(&spool_root);
        let report = ingester.ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 2);
        assert_eq!(report.skipped.len(), 0);
        assert_eq!(report.failed.len(), 0);

        // Verify segments are now in the store.
        let segments = store.list_segments().unwrap();
        assert_eq!(segments.len(), 2);

        // Verify files moved to processed/.
        let incoming_count = std::fs::read_dir(spool_root.join("incoming"))
            .unwrap()
            .count();
        assert_eq!(incoming_count, 0, "incoming/ should be empty after ingest");

        let processed_count = std::fs::read_dir(spool_root.join("processed"))
            .unwrap()
            .count();
        assert_eq!(processed_count, 2, "processed/ should have 2 files");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_deduplicates() {
        let dir = test_dir("ingest-dedup");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);

        // Pre-populate the store with segment 1.
        let seg1 = sealed_segment("jibotmac", 1);
        store.write_segment(&seg1).unwrap();

        // Spool has segment 1 (duplicate) and segment 2 (new).
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();
        writer.write(&sealed_segment("jibotmac", 2)).unwrap();

        let ingester = SpoolIngester::new(&spool_root);
        let report = ingester.ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 1, "only segment 2 should be new");
        assert_eq!(report.skipped.len(), 1, "segment 1 should be skipped as duplicate");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_detects_corruption() {
        let dir = test_dir("ingest-corrupt");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        // Write a valid segment, then corrupt it.
        let writer = SpoolWriter::new(&spool_root);
        let seg = sealed_segment("jibotmac", 1);
        writer.write(&seg).unwrap();

        // Tamper with the file.
        let path = spool_root.join("incoming/jibotmac-000001.json");
        let mut content = std::fs::read_to_string(&path).unwrap();
        // Change the checksum to be wrong.
        content = content.replace(
            &format!("\"checksum\": {}", seg.checksum),
            "\"checksum\": 99999",
        );
        std::fs::write(&path, content).unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let ingester = SpoolIngester::new(&spool_root);
        let report = ingester.ingest(&store).unwrap();

        assert_eq!(report.failed.len(), 1, "corrupt segment should fail");
        assert!(report.failed[0].1.contains("checksum"), "error should mention checksum");

        // Corrupt file should still be in incoming/ (not moved).
        assert!(path.exists(), "failed segment should stay in incoming/");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_empty_spool() {
        let dir = test_dir("ingest-empty");
        let ledger_root = dir.join("ledger");

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let ingester = SpoolIngester::new(dir.join("nonexistent-spool"));
        let report = ingester.ingest(&store).unwrap();

        assert_eq!(report.total(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_full_pipeline_writer_to_ingester() {
        // End-to-end: producer writes, spool replicates (simulated),
        // authority ingests and commits.
        let dir = test_dir("full-pipeline");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        // Producer side.
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();
        writer.write(&sealed_segment("jibotmac", 2)).unwrap();
        writer.write(&sealed_segment("jibotmac", 3)).unwrap();

        // Authority side.
        let store = SegmentStore::new(ZoneId::new("sankosh"), &ledger_root);
        let ingester = SpoolIngester::new(&spool_root);

        let report = ingester.ingest(&store).unwrap();
        report.print_summary();

        assert_eq!(report.committed.len(), 3);
        assert_eq!(report.total(), 3);

        // Store should now have all 3 segments.
        let segments = store.list_segments().unwrap();
        assert_eq!(segments.len(), 3);

        // All segments should verify.
        let failures = store.verify_all().unwrap();
        assert!(failures.is_empty());

        // No gaps.
        let gaps = store.detect_gaps().unwrap();
        assert!(gaps.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
