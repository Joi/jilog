//! Segment files -- append-only containers for events.
//!
//! # Rust concepts in this file
//!
//! - **Vec<T>**: Growable array (like Python list, but typed).
//! - **impl blocks**: Methods live in `impl Segment { ... }`.
//! - **Result<T, E>**: Rust's way of saying "this can fail."
//!   - `Ok(value)` = success, `Err(e)` = failure.
//!   - The `?` operator propagates errors upward (like Python's
//!     `raise` but automatic). `let x = risky_thing()?;` either
//!     unwraps the Ok value or returns the Err to the caller.
//! - **Path vs PathBuf**: `&Path` is a borrowed reference (like &str),
//!   `PathBuf` is an owned path (like String). Use `&Path` in function
//!   arguments, `PathBuf` when you need to store or build paths.
//! - **std::fs**: File I/O. `read_to_string` reads a whole file,
//!   `write` atomically creates/overwrites. `create_dir_all` is mkdir -p.
//! - **&self vs &mut self**: `&self` borrows immutably (read-only),
//!   `&mut self` borrows mutably (read-write). Rust enforces at compile
//!   time that you can't have both at once.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::LedgerError;
use crate::event::Event;

// ---------------------------------------------------------------------------
// Segment: the authoritative storage unit
// ---------------------------------------------------------------------------

/// An append-only segment containing an ordered list of events.
///
/// Segments are the **authoritative** storage unit. Each segment file
/// is self-describing and can be validated independently.
///
/// # Lifecycle
///
/// 1. `Segment::new(source, seq)` — create an empty segment.
/// 2. `segment.append(event)` — add events (order matters).
/// 3. `segment.seal()` — compute the checksum (call once, before writing).
/// 4. `segment.write_to_file(path)` — write to disk as JSON.
///
/// To read back: `Segment::read_from_file(path)` → then `segment.verify()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Source system that produced this segment (e.g., "macazbd").
    pub source: String,

    /// Monotonic sequence number for this source. Starts at 1.
    /// Within a zone, no two segments from the same source may share a seq.
    pub source_seq: u64,

    /// CRC32 checksum of the serialized events (for integrity).
    /// Set to 0 until `seal()` is called.
    pub checksum: u32,

    /// When this segment was created (set automatically by `new()`).
    pub created_at: DateTime<Utc>,

    /// The events in this segment, in append order.
    pub events: Vec<Event>,
}

impl Segment {
    /// Create a new empty segment.
    ///
    /// `source` identifies which machine/system produced this segment.
    /// `source_seq` must be the next number in the monotonic sequence.
    ///
    /// # Rust note: `impl Into<String>`
    ///
    /// This parameter type means "anything that can be converted into a
    /// String." You can pass a `&str`, a `String`, or a `Cow<str>` and
    /// it will just work. It's a common Rust ergonomics pattern.
    pub fn new(source: impl Into<String>, source_seq: u64) -> Self {
        Self {
            source: source.into(),
            source_seq,
            checksum: 0,
            created_at: Utc::now(),
            events: Vec::new(),
        }
    }

    /// Append an event to this segment.
    ///
    /// Events should be appended in chronological order. The segment
    /// does not enforce ordering -- that's the caller's responsibility.
    ///
    /// # Rust note: `&mut self`
    ///
    /// `&mut self` means "I need exclusive, mutable access to this
    /// segment." Rust won't let anyone else read or write it while
    /// this borrow is active. This prevents data races at compile time.
    pub fn append(&mut self, event: Event) {
        self.events.push(event);
    }

    /// How many events are in this segment.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Is this segment empty?
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    // -----------------------------------------------------------------------
    // Integrity
    // -----------------------------------------------------------------------

    /// Compute and store the CRC32 checksum. Call once before writing to disk.
    ///
    /// "Sealing" a segment means "I'm done appending events, compute
    /// the integrity checksum." After this, `verify()` will confirm
    /// the events haven't been tampered with.
    ///
    /// # Rust note: `map_err`
    ///
    /// `serde_json::to_vec` returns `Result<Vec<u8>, serde_json::Error>`.
    /// Our function returns `Result<(), LedgerError>`. The `map_err`
    /// call converts the serde error into our error type. It's like
    /// a `try/except` that re-wraps the exception.
    pub fn seal(&mut self) -> Result<(), LedgerError> {
        let bytes = serde_json::to_vec(&self.events)
            .map_err(|e| LedgerError::Serialization(e.to_string()))?;
        self.checksum = crc32fast::hash(&bytes);
        Ok(())
    }

    /// Verify the checksum matches the current events.
    ///
    /// Returns `Ok(true)` if valid, `Ok(false)` if checksum mismatch.
    /// Returns `Err` only if serialization itself fails.
    pub fn verify(&self) -> Result<bool, LedgerError> {
        let bytes = serde_json::to_vec(&self.events)
            .map_err(|e| LedgerError::Serialization(e.to_string()))?;
        Ok(crc32fast::hash(&bytes) == self.checksum)
    }

    // -----------------------------------------------------------------------
    // File I/O
    // -----------------------------------------------------------------------

    /// Resolve to an absolute path (so parent-chain derivation works
    /// even for bare relative paths), create the parent directories,
    /// and best-effort fsync the created chain (leaf parent + its
    /// parent) so the new directories themselves survive a crash.
    fn prepare_target(path: &Path) -> Result<std::path::PathBuf, LedgerError> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
            // Best-effort durability for the directory CHAIN just
            // ensured: fsync the leaf parent and its parent. Failure is
            // logged, not fatal — the file content + final publish
            // fsyncs below carry the hard guarantee.
            Self::fsync_dir_best_effort(parent);
            if let Some(grand) = parent.parent() {
                Self::fsync_dir_best_effort(grand);
            }
        }
        Ok(abs)
    }

    /// Best-effort directory fsync (unix); logs on failure.
    fn fsync_dir_best_effort(dir: &Path) {
        #[cfg(unix)]
        if let Err(e) = fs::File::open(dir).and_then(|d| d.sync_all()) {
            tracing::warn!(dir = %dir.display(), error = %e, "failed to fsync directory");
        }
        #[cfg(not(unix))]
        let _ = dir;
    }

    /// Create a brand-new file, retrying with fresh candidate names
    /// (from `namegen`, called with the attempt number) whenever a
    /// candidate already exists. `create_new` is load-bearing: an
    /// existing file at a candidate name — e.g. a crash-left tmp that
    /// is already hard-linked to a PUBLISHED destination — must never
    /// be reopened or truncated; it gets skipped, untouched.
    fn create_new_with_retry(
        namegen: &mut dyn FnMut(u32) -> std::path::PathBuf,
        attempts: u32,
    ) -> Result<(std::path::PathBuf, fs::File), LedgerError> {
        for attempt in 0..attempts {
            let candidate = namegen(attempt);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(f) => return Ok((candidate, f)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(LedgerError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("could not create a unique tmp file after {attempts} attempts"),
        )))
    }

    /// Serialize this segment and write it to a uniquely-named
    /// `<path>.<pid>.<counter>.<nanos>.tmp` sibling, fsynced. Returns
    /// the tmp path; the caller must link/rename it into place (and
    /// clean it up on failure).
    fn write_tmp_synced(&self, path: &Path) -> Result<std::path::PathBuf, LedgerError> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        // Serialize to pretty JSON for human readability.
        // In production you might use compact JSON for size.
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| LedgerError::Serialization(e.to_string()))?;

        // Unique, unpredictable same-directory tmp sibling: `foo.json`
        // -> `foo.json.<pid>.<counter>.<nanos>.tmp`. A full-name suffix
        // (NOT extension replacement) keeps it out of `.json` filters;
        // pid + a process-wide counter + a clock-nanos component keep
        // the name from ever being deterministic, and create_new (in
        // create_new_with_retry) guarantees a leftover file at any
        // colliding name is skipped, never truncated.
        static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let (tmp, mut file) = Self::create_new_with_retry(
            &mut |_attempt| {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                let mut tmp_name = path.as_os_str().to_os_string();
                tmp_name.push(format!(
                    ".{}.{}.{}.tmp",
                    std::process::id(),
                    TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
                    nanos
                ));
                std::path::PathBuf::from(tmp_name)
            },
            16,
        )?;

        let write_synced = (|| -> std::io::Result<()> {
            file.write_all(json.as_bytes())?;
            file.sync_all()
        })();
        if let Err(e) = write_synced {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(tmp)
    }

    /// Write this segment to a JSON file, atomically and durably, with
    /// REPLACE semantics (an existing file at `path` is overwritten).
    /// Publication paths that must never clobber concurrent writers —
    /// spool `incoming/`, store commits, `processed/` moves — use
    /// [`Segment::publish_new`] instead; this method is for repair /
    /// rewrite flows where replacing is the point.
    ///
    /// # Durability guarantee (unix)
    ///
    /// - the path is resolved to absolute first, so parent derivation
    ///   works for bare relative paths;
    /// - after `create_dir_all`, the parent and grandparent directories
    ///   are fsynced best-effort (failure logged, not fatal);
    /// - the bytes are written to a uniquely-named `<path>.<pid>.<n>.tmp`
    ///   sibling and fsynced BEFORE the rename (hard error on failure);
    /// - `fs::rename` makes publication atomic: readers see the old
    ///   state or the complete new file, never a truncation;
    /// - the parent directory is fsynced after the rename (hard error
    ///   on failure), making the directory entry durable.
    ///
    /// Consumers must ignore `*.tmp` files (the spool ingester's `.json`
    /// extension filter already does).
    ///
    /// # Rust note: `AsRef<Path>`
    ///
    /// `impl AsRef<Path>` means "anything that can be used as a file
    /// path." This includes `&str`, `String`, `&Path`, and `PathBuf`.
    /// It's how Rust's standard library achieves the same ergonomics
    /// as Python's `os.path` accepting both strings and Path objects.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> Result<(), LedgerError> {
        let path = Self::prepare_target(path.as_ref())?;
        let tmp = self.write_tmp_synced(&path)?;
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            // Directory fsync: File::open on a dir + sync_all is the
            // portable-unix way to persist the new directory entry.
            fs::File::open(parent).and_then(|d| d.sync_all())?;
        }
        Ok(())
    }

    /// Publish this segment to `path` atomically, durably, and WITHOUT
    /// clobbering: if `path` already exists — even one created in the
    /// races that a bidirectionally-synced directory makes real — the
    /// existing file is never replaced.
    ///
    /// Mechanism: write the fsynced unique tmp sibling, then
    /// `fs::hard_link(tmp, path)` — which, unlike rename, FAILS with
    /// `AlreadyExists` instead of replacing — and remove the tmp. On
    /// `AlreadyExists` the existing file is read and content-compared:
    /// identical -> `Ok(PublishOutcome::AlreadyIdentical)` (idempotent
    /// skip), different -> an `IntegrityFailure` error, with BOTH files
    /// left intact for the operator. Durability is as documented on
    /// [`Segment::write_to_file`] (same tmp-fsync + parent-dir fsync).
    ///
    /// # Filesystem requirement: hard links
    ///
    /// The no-clobber guarantee comes from `hard_link`, so the target
    /// filesystem MUST support hard links. exFAT/FAT volumes and some
    /// SMB/NFS/FUSE mounts do not; there, every publish — including
    /// plain `SegmentStore::write_segment` commits, which route through
    /// here — fails with an error naming this requirement. Ledgers must
    /// live on a hard-link-capable filesystem (APFS, HFS+, ext4, ...).
    pub fn publish_new(&self, path: impl AsRef<Path>) -> Result<PublishOutcome, LedgerError> {
        let path = Self::prepare_target(path.as_ref())?;
        let tmp = self.write_tmp_synced(&path)?;
        match fs::hard_link(&tmp, &path) {
            Ok(()) => {
                // The tmp is now an ALIAS of the published file; a
                // failure to remove it must surface (a surviving alias
                // could be mistaken for a scratch file later), it is
                // not ignorable cleanup.
                let cleanup = fs::remove_file(&tmp);
                #[cfg(unix)]
                if let Some(parent) = path.parent() {
                    fs::File::open(parent).and_then(|d| d.sync_all())?;
                }
                if let Err(e) = cleanup {
                    return Err(LedgerError::Io(std::io::Error::new(
                        e.kind(),
                        format!(
                            "segment published to {} but its tmp alias {} could not be \
                             removed: {e} — remove the alias manually",
                            path.display(),
                            tmp.display()
                        ),
                    )));
                }
                Ok(PublishOutcome::Published)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp);
                let existing = Segment::read_from_file(&path)?;
                if existing.content_matches(self) {
                    Ok(PublishOutcome::AlreadyIdentical)
                } else {
                    Err(LedgerError::IntegrityFailure(format!(
                        "no-clobber publish: {} already exists with DIFFERENT \
                         content — not overwriting",
                        path.display()
                    )))
                }
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                // A bare EPERM/ENOTSUP here is undiagnosable; name the
                // hard-link requirement (exFAT/FAT and some network
                // mounts lack it) instead of passing it through raw.
                Err(LedgerError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "no-clobber publish of {} failed at hard_link: {e} — publishing \
                         requires a filesystem with hard-link support (exFAT/FAT and \
                         some SMB/NFS/FUSE mounts do not have it)",
                        path.display()
                    ),
                )))
            }
        }
    }

    /// Deep content equality with another segment: identity (source,
    /// seq), stored checksum, creation timestamp, AND the full event
    /// list — every serialized field. Spool duplicate/conflict
    /// detection uses this instead of trusting the 32-bit CRC alone.
    pub fn content_matches(&self, other: &Segment) -> bool {
        self.source == other.source
            && self.source_seq == other.source_seq
            && self.checksum == other.checksum
            && self.created_at == other.created_at
            && self.events == other.events
    }

    /// Read a segment from a JSON file.
    ///
    /// This does NOT automatically verify the checksum. Call `.verify()`
    /// after reading if you need integrity confirmation.
    ///
    /// # Rust note: no `self` parameter
    ///
    /// This is an "associated function" (like a Python classmethod or
    /// staticmethod). You call it as `Segment::read_from_file(path)`,
    /// not `segment.read_from_file(path)`. It constructs a new Segment
    /// from the file contents.
    pub fn read_from_file(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let contents = fs::read_to_string(path)?;
        let segment: Segment = serde_json::from_str(&contents)
            .map_err(|e| LedgerError::Serialization(e.to_string()))?;
        Ok(segment)
    }

    /// Generate the canonical filename for this segment.
    ///
    /// Format: `{source}-{seq:06}.json`
    /// Example: `macazbd-000001.json`
    ///
    /// The zero-padded sequence number ensures lexicographic sort order
    /// matches chronological order (up to 999,999 segments per source).
    pub fn filename(&self) -> String {
        format!("{}-{:06}.json", self.source, self.source_seq)
    }
}

/// Outcome of a [`Segment::publish_new`] no-clobber publication.
#[derive(Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The segment was written; the destination did not exist before.
    Published,
    /// The destination already held a content-identical copy; nothing
    /// was written (idempotent skip).
    AlreadyIdentical,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventClass, PayloadTier};
    use uuid::Uuid;

    /// Helper: create a minimal test event.
    ///
    /// # Rust note: `#[cfg(test)]`
    ///
    /// The `#[cfg(test)]` attribute on the `mod tests` block means this
    /// code only compiles when running `cargo test`. It doesn't exist
    /// in the production binary. This is Rust's equivalent of Python's
    /// `if __name__ == "__main__"` but enforced by the compiler.
    fn test_event(zone: &str, seq: u64) -> Event {
        Event {
            event_id: Uuid::now_v7(),
            zone: zone.to_string(),
            source: "test".to_string(),
            source_seq: seq,
            timestamp: Utc::now(),
            correlation_id: None,
            causation_id: None,
            actor_ref: Some("person:test-user".to_string()),
            object_ref: None,
            event_class: EventClass::Health,
            payload_tier: PayloadTier::MetadataOnly,
            payload: None,
        }
    }

    #[test]
    fn test_new_segment_is_empty() {
        let seg = Segment::new("macazbd", 1);
        assert!(seg.is_empty());
        assert_eq!(seg.len(), 0);
        assert_eq!(seg.source, "macazbd");
        assert_eq!(seg.source_seq, 1);
        assert_eq!(seg.checksum, 0);
    }

    #[test]
    fn test_append_and_len() {
        let mut seg = Segment::new("macazbd", 1);
        seg.append(test_event("public-ops", 1));
        seg.append(test_event("public-ops", 2));
        assert_eq!(seg.len(), 2);
        assert!(!seg.is_empty());
    }

    #[test]
    fn test_seal_and_verify() {
        let mut seg = Segment::new("macazbd", 1);
        seg.append(test_event("public-ops", 1));
        seg.seal().unwrap();

        // Checksum should now be non-zero.
        assert_ne!(seg.checksum, 0);

        // Verify should pass.
        assert!(seg.verify().unwrap());
    }

    #[test]
    fn test_verify_detects_tampering() {
        let mut seg = Segment::new("macazbd", 1);
        seg.append(test_event("public-ops", 1));
        seg.seal().unwrap();

        // Tamper: add another event after sealing.
        seg.events.push(test_event("public-ops", 2));

        // Verify should now fail.
        assert!(!seg.verify().unwrap());
    }

    #[test]
    fn test_filename_format() {
        let seg = Segment::new("macazbd", 1);
        assert_eq!(seg.filename(), "macazbd-000001.json");

        let seg2 = Segment::new("jibotmac", 42);
        assert_eq!(seg2.filename(), "jibotmac-000042.json");
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        // # Rust note: `tempfile`
        //
        // We use a temporary directory so tests don't leave files around.
        // `std::env::temp_dir()` gives us the OS temp directory.
        let dir = std::env::temp_dir().join("opsctl-test-segment-roundtrip");
        let _ = fs::remove_dir_all(&dir); // clean up from previous runs
        fs::create_dir_all(&dir).unwrap();

        let mut seg = Segment::new("macazbd", 1);
        seg.append(test_event("public-ops", 1));
        seg.append(test_event("public-ops", 2));
        seg.seal().unwrap();

        // Write to file.
        let path = dir.join(seg.filename());
        seg.write_to_file(&path).unwrap();

        // Read back.
        let loaded = Segment::read_from_file(&path).unwrap();

        // Verify structural equality.
        assert_eq!(loaded.source, "macazbd");
        assert_eq!(loaded.source_seq, 1);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.checksum, seg.checksum);

        // Verify integrity.
        assert!(loaded.verify().unwrap());

        // Clean up.
        let _ = fs::remove_dir_all(&dir);
    }

    /// Count `*.tmp` files in a directory.
    fn tmp_count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count()
    }

    #[test]
    fn test_write_rename_failure_cleans_tmp() {
        let dir = std::env::temp_dir().join("opsctl-test-segment-rename-fail");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut seg = Segment::new("macazbd", 1);
        seg.append(test_event("public-ops", 1));
        seg.seal().unwrap();

        // Force the RENAME (not the tmp write) to fail: the target path
        // is an existing directory, so the tmp file is created and
        // synced, and only the final rename errors.
        let path = dir.join(seg.filename());
        fs::create_dir_all(path.join("occupied")).unwrap();

        let err = seg.write_to_file(&path);
        assert!(err.is_err(), "rename onto a directory must fail");
        assert_eq!(
            tmp_count(&dir),
            0,
            "failed rename must clean up its tmp file"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_concurrent_writers_one_winner_no_stray_tmp() {
        let dir = std::env::temp_dir().join("opsctl-test-segment-concurrent");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Two segments with the same identity but different content,
        // racing on the same target path from many alternating writes.
        // Unique tmp names mean neither writer ever scribbles on the
        // other's tmp; the final file is always one COMPLETE segment.
        let mut a = Segment::new("macazbd", 1);
        a.append(test_event("public-ops", 1));
        a.seal().unwrap();
        let mut b = Segment::new("macazbd", 1);
        b.append(test_event("public-ops", 2));
        b.append(test_event("public-ops", 3));
        b.seal().unwrap();

        let path = dir.join(a.filename());
        std::thread::scope(|s| {
            let (path_a, path_b) = (path.clone(), path.clone());
            let (sa, sb) = (a.clone(), b.clone());
            let ta = s.spawn(move || {
                for _ in 0..50 {
                    sa.write_to_file(&path_a).unwrap();
                }
            });
            let tb = s.spawn(move || {
                for _ in 0..50 {
                    sb.write_to_file(&path_b).unwrap();
                }
            });
            ta.join().unwrap();
            tb.join().unwrap();
        });

        // Exactly one winner file, valid and equal to one of the inputs.
        let loaded = Segment::read_from_file(&path).unwrap();
        assert!(loaded.verify().unwrap(), "winner must be a complete, valid segment");
        assert!(
            loaded.content_matches(&a) || loaded.content_matches(&b),
            "winner must be one of the two written segments, not a mix"
        );
        assert_eq!(tmp_count(&dir), 0, "no stray tmp files after the race");
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "exactly one file remains"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_new_with_retry_never_touches_planted_file() {
        let dir = std::env::temp_dir().join("opsctl-test-segment-createnew");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Plant a file at the FIRST candidate name — simulating a
        // crash-left tmp that may already be hard-linked to a published
        // destination. create_new semantics must skip it (retry with the
        // next name), never reopen or truncate it.
        let planted = dir.join("collide.0.tmp");
        fs::write(&planted, "precious planted bytes").unwrap();

        let mut generated = Vec::new();
        let (chosen, file) = Segment::create_new_with_retry(
            &mut |attempt| {
                let p = dir.join(format!("collide.{attempt}.tmp"));
                generated.push(p.clone());
                p
            },
            16,
        )
        .unwrap();
        drop(file);

        assert_eq!(chosen, dir.join("collide.1.tmp"), "collision must retry with a fresh name");
        assert_eq!(generated.len(), 2, "exactly one retry");
        assert_eq!(
            fs::read_to_string(&planted).unwrap(),
            "precious planted bytes",
            "planted file must be untouched (no truncate, no reopen)"
        );

        // Bounded: all candidates taken -> error, planted files intact.
        let err = Segment::create_new_with_retry(&mut |_| planted.clone(), 3).unwrap_err();
        assert!(
            err.to_string().contains("3 attempts"),
            "unexpected error: {err}"
        );
        assert_eq!(fs::read_to_string(&planted).unwrap(), "precious planted bytes");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_content_matches_detects_differing_events() {
        let mut a = Segment::new("macazbd", 1);
        a.append(test_event("public-ops", 1));
        a.seal().unwrap();
        let b = a.clone();
        assert!(a.content_matches(&b));

        // Same identity, same FORGED checksum, different events.
        let mut c = Segment::new("macazbd", 1);
        c.append(test_event("public-ops", 2));
        c.seal().unwrap();
        c.checksum = a.checksum;
        assert!(!a.content_matches(&c), "event content must be compared, not just CRC");
    }

    #[test]
    fn test_content_matches_detects_differing_created_at() {
        let mut a = Segment::new("macazbd", 1);
        a.append(test_event("public-ops", 1));
        a.seal().unwrap();

        // EVERYTHING identical except created_at.
        let mut b = a.clone();
        b.created_at = b.created_at + chrono::Duration::seconds(1);
        assert!(
            !a.content_matches(&b),
            "created_at is part of segment content and must be compared"
        );
    }

    #[test]
    fn test_publish_new_no_clobber() {
        let dir = std::env::temp_dir().join("opsctl-test-segment-publish");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut a = Segment::new("macazbd", 1);
        a.append(test_event("public-ops", 1));
        a.seal().unwrap();
        let mut b = Segment::new("macazbd", 1);
        b.append(test_event("public-ops", 2));
        b.seal().unwrap();

        let path = dir.join(a.filename());

        // Fresh publish.
        assert_eq!(a.publish_new(&path).unwrap(), PublishOutcome::Published);
        // Identical re-publish: idempotent skip.
        assert_eq!(a.publish_new(&path).unwrap(), PublishOutcome::AlreadyIdentical);
        // Different content at the same path: error, existing file intact.
        let err = b.publish_new(&path).unwrap_err();
        assert!(
            err.to_string().contains("DIFFERENT content"),
            "unexpected error: {err}"
        );
        let on_disk = Segment::read_from_file(&path).unwrap();
        assert!(on_disk.content_matches(&a), "conflict must not clobber the existing file");
        assert_eq!(tmp_count(&dir), 0, "no tmp leftovers after any outcome");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_nonexistent_file_returns_io_error() {
        let result = Segment::read_from_file("/tmp/does-not-exist-opsctl.json");

        // # Rust note: pattern matching on errors
        //
        // `matches!` is a macro that checks if a value matches a
        // pattern. It's like `isinstance()` but for enum variants.
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LedgerError::Io(_)));
    }
}
