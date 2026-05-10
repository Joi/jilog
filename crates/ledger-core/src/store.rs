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

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

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

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Full path for a segment file.
    fn segment_path(&self, segment: &Segment) -> PathBuf {
        self.segments_dir().join(segment.filename())
    }
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
