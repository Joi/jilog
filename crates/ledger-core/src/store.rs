//! Segment store -- manages a directory of segments for one zone.
//!
//! # Rust concepts in this file
//!
//! - **PathBuf**: An owned, mutable filesystem path. Like `String` is to
//!   `&str`, `PathBuf` is to `&Path`. You need PathBuf when storing a
//!   path in a struct (because the struct owns the data).
//!
//! - **Iterators**: Rust's equivalent of Python generators. Methods like
//!   `.filter()`, `.map()`, `.collect()` chain together into a pipeline.
//!   Nothing runs until `.collect()` (or another terminal op) is called.
//!   This is "lazy evaluation" and it's very efficient.
//!
//! - **Closures**: `|x| x + 1` is a closure (anonymous function).
//!   In `.filter(|entry| entry.path().extension() == ...)`, the `|entry|`
//!   part captures the loop variable. Closures in Rust automatically
//!   borrow or move captured variables -- the compiler figures it out.
//!
//! - **Sorting**: `.sort_by_key(|s| s.source_seq)` sorts a Vec in place.
//!   Unlike Python's `sorted()` which returns a new list, `.sort()` and
//!   `.sort_by_key()` mutate the Vec. This avoids allocation.
//!
//! - **BTreeMap**: A sorted map (like Python's dict but keys are always
//!   in order). We use it for tracking the latest sequence number per
//!   source, so iteration is deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::LedgerError;
use crate::segment::Segment;
use crate::zone::ZoneId;

// ---------------------------------------------------------------------------
// SegmentStore: one zone's segment directory
// ---------------------------------------------------------------------------

/// Manages the segment file directory for a single zone.
///
/// # Directory layout
///
/// ```text
/// {ledger_path}/
///   segments/
///     macazbd-000001.json
///     macazbd-000002.json
///     jibotmac-000001.json
/// ```
///
/// Each zone has its own SegmentStore. Stores are independent and
/// do not know about each other (zone isolation).
pub struct SegmentStore {
    /// Which zone this store belongs to.
    pub zone: ZoneId,

    /// Root directory for this zone's ledger files.
    root: PathBuf,
}

impl SegmentStore {
    /// Create a new store for the given zone and root directory.
    ///
    /// Does NOT create the directory yet -- call `ensure_dirs()` for that.
    ///
    /// # Rust note: `Into<PathBuf>`
    ///
    /// Same pattern as `Into<String>` -- accepts anything path-like.
    /// Callers can pass `"/some/path"`, `String`, `Path`, or `PathBuf`.
    pub fn new(zone: ZoneId, root: impl Into<PathBuf>) -> Self {
        Self {
            zone,
            root: root.into(),
        }
    }

    /// Create the segment directory if it doesn't exist.
    pub fn ensure_dirs(&self) -> Result<(), LedgerError> {
        fs::create_dir_all(self.segments_dir())?;
        Ok(())
    }

    /// Path to the segments subdirectory.
    fn segments_dir(&self) -> PathBuf {
        self.root.join("segments")
    }

    // -----------------------------------------------------------------------
    // Write operations
    // -----------------------------------------------------------------------

    /// Write a sealed segment to the store.
    ///
    /// Checks for:
    /// - Duplicate segments (same source + seq already exists)
    /// - Sequence gaps (warns but doesn't reject)
    ///
    /// The segment MUST be sealed before writing. If checksum is 0,
    /// this returns an error.
    ///
    /// # Rust note: ownership vs borrowing
    ///
    /// This takes `&self` (immutable borrow of the store) and `&Segment`
    /// (immutable borrow of the segment). We only need to read both --
    /// the actual mutation is the filesystem write, which doesn't
    /// require `&mut self` because it's an external side effect.
    pub fn write_segment(&self, segment: &Segment) -> Result<(), LedgerError> {
        // Refuse to write unsealed segments.
        if segment.checksum == 0 && !segment.events.is_empty() {
            return Err(LedgerError::IntegrityFailure(
                "segment must be sealed before writing (checksum is 0)".to_string(),
            ));
        }

        let path = self.segment_path(segment);

        // Check for duplicates.
        if path.exists() {
            return Err(LedgerError::DuplicateSegment {
                src: segment.source.clone(),
                seq: segment.source_seq,
            });
        }

        // Check for sequence gaps.
        let latest = self.latest_seq(&segment.source)?;
        let expected = latest + 1;
        if segment.source_seq != expected {
            // Log warning but don't reject -- the caller might be
            // backfilling or the gap might be intentional.
            tracing::warn!(
                source = %segment.source,
                expected = expected,
                got = segment.source_seq,
                "segment sequence gap detected"
            );
        }

        self.ensure_dirs()?;
        segment.write_to_file(&path)?;

        tracing::info!(
            zone = %self.zone,
            source = %segment.source,
            seq = segment.source_seq,
            events = segment.events.len(),
            "segment written"
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Read operations
    // -----------------------------------------------------------------------

    /// Read a specific segment by source and sequence number.
    pub fn read_segment(&self, source: &str, seq: u64) -> Result<Segment, LedgerError> {
        let filename = format!("{}-{:06}.json", source, seq);
        let path = self.segments_dir().join(filename);
        Segment::read_from_file(path)
    }

    /// List all segment files in order (sorted by filename).
    ///
    /// Returns `(source, seq, path)` tuples.
    ///
    /// # Rust note: iterator chains
    ///
    /// This is a classic Rust iterator pipeline:
    /// 1. `fs::read_dir()` — yields directory entries (like `os.listdir`)
    /// 2. `.filter_map()` — maps + filters in one step. `Some(x)` keeps
    ///    the item, `None` skips it. It's like a list comprehension
    ///    with a filter: `[f(x) for x in items if condition]`.
    /// 3. `.collect()` — consumes the iterator into a Vec.
    ///
    /// Without `.collect()`, nothing happens -- iterators are lazy.
    pub fn list_segments(&self) -> Result<Vec<(String, u64, PathBuf)>, LedgerError> {
        let dir = self.segments_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries: Vec<(String, u64, PathBuf)> = fs::read_dir(&dir)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();

                // Only .json files.
                if path.extension()?.to_str()? != "json" {
                    return None;
                }

                // Parse filename: "{source}-{seq:06}.json"
                let stem = path.file_stem()?.to_str()?;
                let dash_pos = stem.rfind('-')?;
                let source = stem[..dash_pos].to_string();
                let seq: u64 = stem[dash_pos + 1..].parse().ok()?;

                Some((source, seq, path))
            })
            .collect();

        // Sort by source, then by sequence number.
        entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        Ok(entries)
    }

    /// Read ALL segments, in order. Useful for rebuilding projections.
    ///
    /// # Rust note: collecting Results
    ///
    /// `.collect::<Result<Vec<_>, _>>()` is a powerful pattern. If ANY
    /// item in the iterator is `Err`, the whole collect fails with that
    /// error. If all are `Ok`, you get a `Vec` of the unwrapped values.
    /// It's like: "try to collect all of these, but bail on first error."
    pub fn read_all_segments(&self) -> Result<Vec<Segment>, LedgerError> {
        let entries = self.list_segments()?;

        entries
            .into_iter()
            .map(|(_, _, path)| Segment::read_from_file(path))
            .collect()
    }

    /// Get the latest sequence number for a given source.
    ///
    /// Returns 0 if no segments exist for that source (so the first
    /// segment should have `source_seq = 1`).
    pub fn latest_seq(&self, source: &str) -> Result<u64, LedgerError> {
        let entries = self.list_segments()?;

        let max_seq = entries
            .iter()
            .filter(|(src, _, _)| src == source)
            .map(|(_, seq, _)| *seq)
            .max()
            .unwrap_or(0);

        Ok(max_seq)
    }

    /// Get the latest sequence number for each source.
    ///
    /// Returns a BTreeMap so the output is deterministic (sorted by key).
    pub fn latest_seqs(&self) -> Result<BTreeMap<String, u64>, LedgerError> {
        let entries = self.list_segments()?;

        let mut seqs = BTreeMap::new();
        for (source, seq, _) in entries {
            let entry = seqs.entry(source).or_insert(0u64);
            if seq > *entry {
                *entry = seq;
            }
        }

        Ok(seqs)
    }

    // -----------------------------------------------------------------------
    // Integrity
    // -----------------------------------------------------------------------

    /// Verify all segments in the store.
    ///
    /// Returns a list of `(source, seq, error_message)` for any failures.
    /// An empty vec means everything is valid.
    pub fn verify_all(&self) -> Result<Vec<(String, u64, String)>, LedgerError> {
        let entries = self.list_segments()?;
        let mut failures = Vec::new();

        for (source, seq, path) in entries {
            match Segment::read_from_file(&path) {
                Ok(segment) => match segment.verify() {
                    Ok(true) => {} // All good.
                    Ok(false) => {
                        failures.push((source, seq, "checksum mismatch".to_string()));
                    }
                    Err(e) => {
                        failures.push((source, seq, format!("verify error: {e}")));
                    }
                },
                Err(e) => {
                    failures.push((source, seq, format!("read error: {e}")));
                }
            }
        }

        Ok(failures)
    }

    /// Detect sequence gaps for each source.
    ///
    /// Returns a list of `(source, missing_seq)` for each gap found.
    /// For example, if source "macazbd" has segments 1, 2, 4, this
    /// returns `[("macazbd", 3)]`.
    pub fn detect_gaps(&self) -> Result<Vec<(String, u64)>, LedgerError> {
        let entries = self.list_segments()?;
        let mut gaps = Vec::new();

        // Group by source.
        let mut by_source: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for (source, seq, _) in entries {
            by_source.entry(source).or_default().push(seq);
        }

        // Check each source for gaps.
        for (source, mut seqs) in by_source {
            seqs.sort();
            for i in 0..seqs.len() {
                let expected = if i == 0 { 1 } else { seqs[i - 1] + 1 };
                // Fill in all missing between expected and actual.
                for missing in expected..seqs[i] {
                    gaps.push((source.clone(), missing));
                }
            }
        }

        Ok(gaps)
    }

    /// Verify only segments not yet covered by the verification checkpoint,
    /// plus re-check any previously recorded failures.
    ///
    /// Cost is O(new segments) instead of O(all history) — the checkpoint at
    /// `{root}/.verify-checkpoint.json` records, per source, the highest seq
    /// that has verified clean, along with any known failures. A missing or
    /// unreadable checkpoint falls back to a full verify (then rewrites it).
    ///
    /// Limitation: a segment that verified clean once and rots afterwards is
    /// only caught by `verify_full()` — incremental runs never re-read it.
    pub fn verify_incremental(&self) -> Result<VerifyReport, LedgerError> {
        let ckpt = self.load_checkpoint();
        self.verify_with_checkpoint(ckpt)
    }

    /// Re-verify every segment (same cost as `verify_all`) and rewrite the
    /// verification checkpoint from the results.
    pub fn verify_full(&self) -> Result<VerifyReport, LedgerError> {
        self.verify_with_checkpoint(VerifyCheckpoint::default())
    }

    /// Shared engine for the two verify entry points: check everything the
    /// checkpoint doesn't cover (plus recorded failures, so repairs clear and
    /// persistent corruption keeps surfacing), then persist the new checkpoint.
    fn verify_with_checkpoint(&self, ckpt: VerifyCheckpoint) -> Result<VerifyReport, LedgerError> {
        let entries = self.list_segments()?;

        let prior_failures: BTreeSet<(String, u64)> = ckpt
            .failures
            .iter()
            .map(|(src, seq, _)| (src.clone(), *seq))
            .collect();

        let mut verified = ckpt.verified;
        let mut newly_verified = 0usize;
        let mut skipped = 0usize;
        let mut failures: Vec<(String, u64, String)> = Vec::new();

        for (source, seq, path) in entries {
            let watermark = verified.get(&source).copied().unwrap_or(0);
            if seq <= watermark && !prior_failures.contains(&(source.clone(), seq)) {
                skipped += 1;
                continue;
            }

            let failure = match Segment::read_from_file(&path) {
                Ok(segment) => match segment.verify() {
                    Ok(true) => None,
                    Ok(false) => Some("checksum mismatch".to_string()),
                    Err(e) => Some(format!("verify error: {e}")),
                },
                Err(e) => Some(format!("read error: {e}")),
            };

            match failure {
                None => {
                    newly_verified += 1;
                    let w = verified.entry(source.clone()).or_insert(0);
                    if seq > *w {
                        *w = seq;
                    }
                }
                Some(msg) => failures.push((source, seq, msg)),
            }
        }

        self.save_checkpoint(&VerifyCheckpoint {
            version: 1,
            verified,
            failures: failures.clone(),
        })?;

        Ok(VerifyReport {
            newly_verified,
            skipped,
            failures,
        })
    }

    /// Path of the verification checkpoint (sibling of `segments/`, NOT
    /// inside it — `list_segments()` must never pick it up).
    fn checkpoint_path(&self) -> PathBuf {
        self.root.join(".verify-checkpoint.json")
    }

    /// Load the checkpoint; a missing or unreadable file degrades to the
    /// empty checkpoint, which makes the next verify a full one.
    fn load_checkpoint(&self) -> VerifyCheckpoint {
        fs::read_to_string(self.checkpoint_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist the checkpoint atomically (tmp + rename) so a crash mid-write
    /// leaves the old checkpoint intact rather than a truncated one.
    fn save_checkpoint(&self, ckpt: &VerifyCheckpoint) -> Result<(), LedgerError> {
        fs::create_dir_all(&self.root)?;
        let body = serde_json::to_string_pretty(ckpt)
            .map_err(|e| LedgerError::Serialization(e.to_string()))?;
        let tmp = self.root.join(".verify-checkpoint.json.tmp");
        fs::write(&tmp, body)?;
        fs::rename(&tmp, self.checkpoint_path())?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Full path for a segment file.
    fn segment_path(&self, segment: &Segment) -> PathBuf {
        self.segments_dir().join(segment.filename())
    }
}

/// On-disk state behind incremental verification. Private — callers only see
/// `VerifyReport`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct VerifyCheckpoint {
    version: u32,
    /// Per source: highest seq that has verified clean.
    verified: BTreeMap<String, u64>,
    /// Known failures `(source, seq, message)`, re-checked every run.
    failures: Vec<(String, u64, String)>,
}

/// Outcome of a `verify_incremental()` / `verify_full()` pass.
#[derive(Debug)]
pub struct VerifyReport {
    /// Segments read and verified clean during this run.
    pub newly_verified: usize,
    /// Segments skipped because the checkpoint already covers them.
    pub skipped: usize,
    /// All currently known failures as `(source, seq, message)` — includes
    /// failures found this run and unrepaired ones from earlier runs.
    pub failures: Vec<(String, u64, String)>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventClass, PayloadTier};
    use chrono::Utc;
    use uuid::Uuid;

    /// Helper: create a minimal test event.
    fn test_event(zone: &str, seq: u64) -> Event {
        Event {
            event_id: Uuid::now_v7(),
            zone: zone.to_string(),
            source: "test".to_string(),
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

    /// Helper: create a sealed segment with N events.
    fn sealed_segment(source: &str, seq: u64, n_events: usize) -> Segment {
        let mut seg = Segment::new(source, seq);
        for i in 0..n_events {
            seg.append(test_event("test-zone", i as u64));
        }
        seg.seal().unwrap();
        seg
    }

    /// Helper: create a temp directory for tests.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("opsctl-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn test_store_write_and_read() {
        let dir = test_dir("store-write-read");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        let seg = sealed_segment("macazbd", 1, 3);
        store.write_segment(&seg).unwrap();

        // Read it back.
        let loaded = store.read_segment("macazbd", 1).unwrap();
        assert_eq!(loaded.source, "macazbd");
        assert_eq!(loaded.source_seq, 1);
        assert_eq!(loaded.events.len(), 3);
        assert!(loaded.verify().unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_rejects_duplicates() {
        let dir = test_dir("store-duplicates");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        let seg = sealed_segment("macazbd", 1, 1);
        store.write_segment(&seg).unwrap();

        // Writing the same source+seq again should fail.
        let seg2 = sealed_segment("macazbd", 1, 2);
        let result = store.write_segment(&seg2);
        assert!(matches!(
            result.unwrap_err(),
            LedgerError::DuplicateSegment { .. }
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_rejects_unsealed() {
        let dir = test_dir("store-unsealed");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        let mut seg = Segment::new("macazbd", 1);
        seg.append(test_event("test-zone", 1));
        // NOT sealed -- checksum is 0.

        let result = store.write_segment(&seg);
        assert!(matches!(
            result.unwrap_err(),
            LedgerError::IntegrityFailure(_)
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_list_and_ordering() {
        let dir = test_dir("store-list");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        // Write out of order.
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 1)).unwrap();
        store.write_segment(&sealed_segment("jibotmac", 1, 1)).unwrap();

        let entries = store.list_segments().unwrap();
        assert_eq!(entries.len(), 3);

        // Should be sorted: jibotmac-1, macazbd-1, macazbd-2.
        assert_eq!(entries[0].0, "jibotmac");
        assert_eq!(entries[1].0, "macazbd");
        assert_eq!(entries[1].1, 1);
        assert_eq!(entries[2].0, "macazbd");
        assert_eq!(entries[2].1, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_latest_seq() {
        let dir = test_dir("store-latest-seq");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        assert_eq!(store.latest_seq("macazbd").unwrap(), 0);

        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        assert_eq!(store.latest_seq("macazbd").unwrap(), 1);

        store.write_segment(&sealed_segment("macazbd", 2, 1)).unwrap();
        assert_eq!(store.latest_seq("macazbd").unwrap(), 2);

        // Different source stays at 0.
        assert_eq!(store.latest_seq("jibotmac").unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_verify_all() {
        let dir = test_dir("store-verify");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        store.write_segment(&sealed_segment("macazbd", 1, 2)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 3)).unwrap();

        let failures = store.verify_all().unwrap();
        assert!(failures.is_empty(), "expected no failures, got: {:?}", failures);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_detect_gaps() {
        let dir = test_dir("store-gaps");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        // Write 1 and 3, skip 2.
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 3, 1)).unwrap();

        let gaps = store.detect_gaps().unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0], ("macazbd".to_string(), 2));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Helper: write a deliberately corrupt segment file (checksum tampered
    /// after sealing, so `verify()` reports a mismatch on read-back).
    fn write_corrupt_segment(store: &SegmentStore, dir: &PathBuf, source: &str, seq: u64) {
        let mut seg = sealed_segment(source, seq, 1);
        seg.checksum = seg.checksum.wrapping_add(1);
        store.ensure_dirs().unwrap();
        seg.write_to_file(dir.join("segments").join(seg.filename())).unwrap();
    }

    #[test]
    fn test_verify_incremental_first_run_checks_all_and_writes_checkpoint() {
        let dir = test_dir("verify-incr-first");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 2)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 1)).unwrap();

        let report = store.verify_incremental().unwrap();
        assert_eq!(report.newly_verified, 2);
        assert_eq!(report.skipped, 0);
        assert!(report.failures.is_empty());
        assert!(dir.join(".verify-checkpoint.json").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_incremental_skips_previously_verified() {
        let dir = test_dir("verify-incr-skip");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.write_segment(&sealed_segment("jibotmac", 1, 1)).unwrap();

        store.verify_incremental().unwrap();
        let report = store.verify_incremental().unwrap();
        assert_eq!(report.newly_verified, 0);
        assert_eq!(report.skipped, 2);
        assert!(report.failures.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_incremental_checks_only_new_segments() {
        let dir = test_dir("verify-incr-new");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 1)).unwrap();
        store.verify_incremental().unwrap();

        store.write_segment(&sealed_segment("macazbd", 3, 1)).unwrap();
        let report = store.verify_incremental().unwrap();
        assert_eq!(report.newly_verified, 1);
        assert_eq!(report.skipped, 2);
        assert!(report.failures.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_incremental_reports_corrupt_new_segment() {
        let dir = test_dir("verify-incr-corrupt");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.verify_incremental().unwrap();

        write_corrupt_segment(&store, &dir, "macazbd", 2);
        let report = store.verify_incremental().unwrap();
        assert_eq!(report.newly_verified, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, "macazbd");
        assert_eq!(report.failures[0].1, 2);
        assert!(report.failures[0].2.contains("checksum"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_incremental_rechecks_failure_until_repaired() {
        let dir = test_dir("verify-incr-repair");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.verify_incremental().unwrap();
        write_corrupt_segment(&store, &dir, "macazbd", 2);
        store.verify_incremental().unwrap();

        // Still failing on the next run (remembered + re-checked).
        let report = store.verify_incremental().unwrap();
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.newly_verified, 0);

        // Repair by writing the segment correctly, then the failure clears.
        let seg = sealed_segment("macazbd", 2, 1);
        seg.write_to_file(dir.join("segments").join(seg.filename())).unwrap();
        let report = store.verify_incremental().unwrap();
        assert!(report.failures.is_empty(), "repaired segment still failing: {:?}", report.failures);
        assert_eq!(report.newly_verified, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_incremental_corrupt_checkpoint_reverifies_all() {
        let dir = test_dir("verify-incr-badckpt");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 1)).unwrap();
        store.verify_incremental().unwrap();

        fs::write(dir.join(".verify-checkpoint.json"), "not json {").unwrap();
        let report = store.verify_incremental().unwrap();
        assert_eq!(report.newly_verified, 2);
        assert_eq!(report.skipped, 0);
        assert!(report.failures.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_full_rechecks_everything_and_resets_checkpoint() {
        let dir = test_dir("verify-full");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);
        store.write_segment(&sealed_segment("macazbd", 1, 1)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 1)).unwrap();
        store.verify_incremental().unwrap();

        let report = store.verify_full().unwrap();
        assert_eq!(report.newly_verified, 2);
        assert_eq!(report.skipped, 0);
        assert!(report.failures.is_empty());

        // Checkpoint still valid for the next incremental run.
        let report = store.verify_incremental().unwrap();
        assert_eq!(report.newly_verified, 0);
        assert_eq!(report.skipped, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_read_all_segments() {
        let dir = test_dir("store-read-all");
        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        store.write_segment(&sealed_segment("macazbd", 1, 2)).unwrap();
        store.write_segment(&sealed_segment("macazbd", 2, 3)).unwrap();

        let all = store.read_all_segments().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].events.len(), 2);
        assert_eq!(all[1].events.len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }
}
