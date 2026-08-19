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
//! - **No-clobber move (`hard_link` + remove)**: after a segment is
//!   committed, it moves from incoming/ to processed/ via
//!   `std::fs::hard_link` — which FAILS with AlreadyExists instead of
//!   replacing, unlike rename — then the incoming copy is removed.
//!   This creates an audit trail, never silently overwrites a
//!   pre-existing processed/ file, and makes recovery easier if
//!   something goes wrong.
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
    /// 2. Validate the source name and filename/identity match
    ///    (path-traversal + spoof guard — see [`crate::valid_source_name`])
    /// 3. Verify the checksum
    /// 4. Attempt to write to the authoritative store
    /// 5. On success: move to `processed/`
    /// 6. On duplicate with IDENTICAL content: move to `processed/`
    ///    (idempotent); a duplicate identity with DIFFERENT content is a
    ///    failure (possible hostname collision or corrupt store copy)
    /// 7. On failure — including a failed rename to `processed/` — record
    ///    the error in `failed` (a rename failure after commit means the
    ///    segment stays in `incoming/` and is reported as failed so the
    ///    run exits nonzero; the store dedup makes the retry harmless)
    ///
    /// Only `*.json` files are considered; in particular the `*.json.tmp`
    /// siblings left by an interrupted atomic write are ignored.
    /// Syncthing conflict artifacts (`*.sync-conflict-*.json`) are also
    /// skipped (with a warning): they can never pass the filename/identity
    /// check, and treating them as failures would make every ingest run
    /// exit nonzero forever after a single sync hiccup. They stay in
    /// `incoming/` for the operator to inspect and remove.
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
        // Per-entry read_dir errors are surfaced as failures, not
        // silently discarded — an unreadable entry could be a segment.
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&incoming)? {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path.extension().and_then(|x| x.to_str()) == Some("json") {
                        // Syncthing conflict artifacts would fail the
                        // identity check on EVERY run — a permanent red
                        // health check for what is an operator cleanup
                        // item, not a segment. Warn and leave them.
                        let name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
                        if name.contains(".sync-conflict-") {
                            tracing::warn!(
                                file = %name,
                                "ignoring Syncthing conflict artifact in incoming/ — \
                                 inspect and remove it manually"
                            );
                            continue;
                        }
                        files.push(path);
                    }
                }
                Err(e) => {
                    report.failed.push((
                        "(unreadable directory entry)".to_string(),
                        format!("read_dir entry error in incoming/: {e}"),
                    ));
                }
            }
        }

        files.sort();

        for path in &files {
            let filename = path.file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("unknown")
                .to_string();

            match self.ingest_one(path, &filename, store) {
                Ok((outcome, segment)) => {
                    // Move to processed/ — the persistent audit trail that
                    // also stops producers from re-spooling. The move is
                    // NO-CLOBBER (hard_link + remove, never rename): a
                    // pre-existing processed/ file with different content
                    // must not be silently replaced. A failed move leaves
                    // the file in incoming/, so it MUST be reported as a
                    // failure (nonzero exit), not as committed/skipped.
                    let dest = processed.join(&filename);
                    match Self::move_no_clobber(path, &dest, &segment) {
                        Ok(()) => {
                            // Persist the move itself: the store commit was
                            // already fsynced by the segment publish, and
                            // syncing both directories here makes the
                            // incoming/ -> processed/ transition survive a
                            // crash (best-effort: the move has happened,
                            // and an undone move is just retried).
                            #[cfg(unix)]
                            for d in [&processed, &incoming] {
                                if let Err(e) =
                                    fs::File::open(d).and_then(|f| f.sync_all())
                                {
                                    tracing::warn!(
                                        dir = %d.display(),
                                        error = %e,
                                        "failed to fsync spool directory after move"
                                    );
                                }
                            }
                            match outcome {
                                IngestOutcome::Committed => report.committed.push(filename),
                                IngestOutcome::Duplicate => report.skipped.push(filename),
                            }
                        }
                        Err(msg) => {
                            let what = match outcome {
                                IngestOutcome::Committed => "committed to store",
                                IngestOutcome::Duplicate => "duplicate of store copy",
                            };
                            tracing::warn!(
                                file = %filename,
                                error = %msg,
                                "failed to move segment to processed/"
                            );
                            report.failed.push((filename, format!("{what} but {msg}")));
                        }
                    }
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

    /// No-clobber move: `hard_link(src, dest)` — which FAILS with
    /// `AlreadyExists` instead of replacing — then remove `src`. When
    /// `dest` already exists, its content decides: identical to
    /// `segment` means the move already happened (remove `src`,
    /// success); different content is an error with both files intact.
    fn move_no_clobber(src: &Path, dest: &Path, segment: &Segment) -> Result<(), String> {
        match fs::hard_link(src, dest) {
            Ok(()) => fs::remove_file(src)
                .map_err(|e| format!("linked into processed/ but failed to remove incoming copy: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match Segment::read_from_file(dest) {
                    Ok(prior) if prior.content_matches(segment) => fs::remove_file(src)
                        .map_err(|e| {
                            format!(
                                "processed/ already held an identical copy but removing \
                                 the incoming copy failed: {e}"
                            )
                        }),
                    Ok(_) => Err(
                        "processed/ already holds DIFFERENT content for this identity — \
                         not overwriting; left in incoming/"
                            .to_string(),
                    ),
                    Err(e) => Err(format!(
                        "processed/ copy exists but is unreadable ({e}) — refusing to \
                         assume it matches; left in incoming/"
                    )),
                }
            }
            Err(e) => Err(format!("rename to processed/ failed: {e}")),
        }
    }

    /// Ingest a single segment file. `filename` is the actual on-disk
    /// name of the file in `incoming/`. Returns the outcome together
    /// with the parsed segment (the caller's no-clobber move needs it
    /// for content comparison).
    fn ingest_one(
        &self,
        path: &Path,
        filename: &str,
        store: &SegmentStore,
    ) -> Result<(IngestOutcome, Segment), SpoolError> {
        // Read the segment.
        let segment = Segment::read_from_file(path)?;

        // Reject source names that fail the shared pattern. Since
        // `Segment::filename()` derives paths from the source, this is
        // the path-traversal guard for everything downstream (store
        // writes, processed/ moves).
        if !crate::valid_source_name(&segment.source) {
            return Err(SpoolError::InvalidSource {
                name: segment.source.clone(),
            });
        }

        // The file must be named exactly what the segment says it is —
        // a mismatch means a spoofed or mislabeled spool entry.
        let expected = segment.filename();
        if filename != expected {
            return Err(SpoolError::IdentityMismatch {
                found: filename.to_string(),
                expected,
            });
        }

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
            Ok(()) => Ok((IngestOutcome::Committed, segment)),
            Err(ledger_core::LedgerError::DuplicateSegment { .. }) => {
                // Same identity already in the store. Only skip when the
                // CONTENT matches too — a same-name segment with different
                // content means two hosts share a source name (hostname
                // collision) or the store copy rotted, and silently
                // dropping one side would lose events. The comparison is
                // deep (metadata + full event list, not just the 32-bit
                // CRC), and a store copy that fails its own verify() is
                // reported instead of being trusted as "identical".
                let existing = store.read_segment(&segment.source, segment.source_seq)?;
                if !existing.verify()? {
                    return Err(SpoolError::IntegrityFailure {
                        src: segment.source.clone(),
                        seq: segment.source_seq,
                        reason: "store copy of this identity fails checksum \
                                 verification — corrupt store copy; left in incoming/"
                            .to_string(),
                    });
                }
                if existing.content_matches(&segment) {
                    Ok((IngestOutcome::Duplicate, segment))
                } else {
                    Err(SpoolError::IntegrityFailure {
                        src: segment.source.clone(),
                        seq: segment.source_seq,
                        reason: "duplicate identity with DIFFERENT content — possible \
                                 hostname collision or corrupt store copy; left in incoming/"
                            .to_string(),
                    })
                }
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

        // Spool has segment 1 (identical duplicate) and segment 2 (new).
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&seg1).unwrap();
        writer.write(&sealed_segment("jibotmac", 2)).unwrap();

        let ingester = SpoolIngester::new(&spool_root);
        let report = ingester.ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 1, "only segment 2 should be new");
        assert_eq!(report.skipped.len(), 1, "segment 1 should be skipped as duplicate");
        assert_eq!(report.failed.len(), 0);
        // Identical duplicate is drained to processed/.
        assert!(spool_root.join("processed/jibotmac-000001.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_rejects_traversal_shaped_source() {
        let dir = test_dir("ingest-traversal");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        // Hand-craft a segment whose `source` is a path-traversal attempt
        // and drop it into incoming/ under an innocuous filename.
        let seg = sealed_segment("../../evil", 1);
        let incoming = spool_root.join("incoming");
        std::fs::create_dir_all(&incoming).unwrap();
        let path = incoming.join("innocent.json");
        std::fs::write(&path, serde_json::to_string_pretty(&seg).unwrap()).unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        assert_eq!(report.failed.len(), 1, "traversal source must fail");
        assert!(
            report.failed[0].1.contains("invalid segment source"),
            "unexpected error: {}",
            report.failed[0].1
        );
        assert!(path.exists(), "rejected segment stays in incoming/");
        assert!(store.list_segments().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_rejects_filename_identity_mismatch() {
        let dir = test_dir("ingest-identity");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        // Valid source, but the file is named as someone else's segment.
        let seg = sealed_segment("hostA", 1);
        let incoming = spool_root.join("incoming");
        std::fs::create_dir_all(&incoming).unwrap();
        let path = incoming.join("hostB-000001.json");
        std::fs::write(&path, serde_json::to_string_pretty(&seg).unwrap()).unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        assert_eq!(report.failed.len(), 1, "identity mismatch must fail");
        assert!(
            report.failed[0].1.contains("does not match segment identity"),
            "unexpected error: {}",
            report.failed[0].1
        );
        assert!(path.exists(), "rejected segment stays in incoming/");
        assert!(store.list_segments().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_duplicate_identity_different_content_fails() {
        let dir = test_dir("ingest-dup-content");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        // Store holds jibotmac-000001 with one content...
        store.write_segment(&sealed_segment("jibotmac", 1)).unwrap();
        // ...and the spool holds the same identity with DIFFERENT content
        // (sealed_segment generates fresh event ids/timestamps each call).
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();

        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 0);
        assert_eq!(report.skipped.len(), 0);
        assert_eq!(report.failed.len(), 1, "conflicting duplicate must fail the run");
        assert!(
            report.failed[0].1.contains("DIFFERENT content"),
            "unexpected error: {}",
            report.failed[0].1
        );
        assert!(
            spool_root.join("incoming/jibotmac-000001.json").exists(),
            "conflicting segment stays in incoming/"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_duplicate_with_corrupt_store_copy_fails() {
        let dir = test_dir("ingest-dup-corrupt-store");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let seg = sealed_segment("jibotmac", 1);
        store.write_segment(&seg).unwrap();
        // Rot the STORE copy (checksum field tampered so verify() fails).
        let store_path = ledger_root.join("segments/jibotmac-000001.json");
        let content = std::fs::read_to_string(&store_path).unwrap().replace(
            &format!("\"checksum\": {}", seg.checksum),
            "\"checksum\": 99999",
        );
        std::fs::write(&store_path, content).unwrap();

        // The spool offers a VALID copy of the same identity.
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&seg).unwrap();

        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();
        assert_eq!(report.skipped.len(), 0, "corrupt store copy must not be trusted");
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].1.contains("fails checksum verification"),
            "unexpected error: {}",
            report.failed[0].1
        );
        assert!(
            spool_root.join("incoming/jibotmac-000001.json").exists(),
            "the good copy stays in incoming/ for recovery"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_ignores_stray_tmp_files() {
        let dir = test_dir("ingest-tmp");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();
        // Simulate an interrupted atomic write: a stray *.json.tmp sibling.
        std::fs::write(
            spool_root.join("incoming/jibotmac-000002.json.tmp"),
            "{ truncated",
        )
        .unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 1);
        assert_eq!(report.failed.len(), 0, "tmp file must not be treated as a segment");
        assert!(
            spool_root.join("incoming/jibotmac-000002.json.tmp").exists(),
            "tmp file is left alone"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_skips_syncthing_conflict_artifacts() {
        let dir = test_dir("ingest-sync-conflict");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();
        // A Syncthing conflict artifact: valid segment JSON, but its name
        // can never match its identity — must be skipped, not failed.
        let conflict = spool_root
            .join("incoming/jibotmac-000002.sync-conflict-20260819-070859-ABCDEFG.json");
        std::fs::write(
            &conflict,
            serde_json::to_string_pretty(&sealed_segment("jibotmac", 2)).unwrap(),
        )
        .unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 1, "the real segment still ingests");
        assert_eq!(
            report.failed.len(),
            0,
            "a sync-conflict artifact must not redden every run: {:?}",
            report.failed
        );
        assert!(conflict.exists(), "artifact is left in incoming/ for the operator");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_preexisting_conflicting_processed_copy_fails() {
        let dir = test_dir("ingest-processed-conflict");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        // processed/ already holds a DIFFERENT segment under this name.
        let planted = sealed_segment("jibotmac", 1);
        planted
            .write_to_file(spool_root.join("processed/jibotmac-000001.json"))
            .unwrap();

        // A different same-identity segment arrives in incoming/.
        let arriving = sealed_segment("jibotmac", 1);
        let writer = SpoolWriter::new(&spool_root);
        writer.write(&arriving).unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        assert_eq!(report.committed.len(), 0, "conflicted move must not report committed");
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].1.contains("DIFFERENT content"),
            "unexpected error: {}",
            report.failed[0].1
        );
        // Both files intact: processed/ copy unchanged, incoming/ kept.
        let on_disk =
            Segment::read_from_file(spool_root.join("processed/jibotmac-000001.json")).unwrap();
        assert!(on_disk.content_matches(&planted), "processed/ copy must be preserved");
        let kept =
            Segment::read_from_file(spool_root.join("incoming/jibotmac-000001.json")).unwrap();
        assert!(kept.content_matches(&arriving), "incoming/ copy must be preserved");
        // The commit itself DID land in the store (failure is about the
        // audit-trail move, not the data).
        assert_eq!(store.list_segments().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_ingest_failed_processed_rename_is_reported_failed() {
        use std::os::unix::fs::PermissionsExt;

        // Read-only permissions cannot induce failures as root (root
        // bypasses permission checks) — skip instead of failing spuriously.
        if ledger_core::test_support::running_as_root() {
            eprintln!("skipping: running as root");
            return;
        }

        let dir = test_dir("ingest-ro-processed");
        let spool_root = dir.join("spool");
        let ledger_root = dir.join("ledger");

        let writer = SpoolWriter::new(&spool_root);
        writer.write(&sealed_segment("jibotmac", 1)).unwrap();

        // Read-only processed/ makes the post-commit rename fail.
        let processed = spool_root.join("processed");
        std::fs::create_dir_all(&processed).unwrap();
        std::fs::set_permissions(&processed, std::fs::Permissions::from_mode(0o555)).unwrap();

        let store = SegmentStore::new(ZoneId::new("test"), &ledger_root);
        let report = SpoolIngester::new(&spool_root).ingest(&store).unwrap();

        // Restore permissions before asserting so cleanup always works.
        std::fs::set_permissions(&processed, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(report.committed.len(), 0, "rename failure must not count as committed");
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].1.contains("rename to processed/ failed"),
            "unexpected error: {}",
            report.failed[0].1
        );
        // Segment IS in the store (commit happened), file stays in incoming/.
        assert_eq!(store.list_segments().unwrap().len(), 1);
        assert!(spool_root.join("incoming/jibotmac-000001.json").exists());

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
