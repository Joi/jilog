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

    /// Write this segment to a JSON file.
    ///
    /// The file is written atomically-ish: we serialize to a string,
    /// then write the whole thing. For true atomic writes (important
    /// on crash), a later phase can add write-to-temp + rename.
    ///
    /// # Rust note: `AsRef<Path>`
    ///
    /// `impl AsRef<Path>` means "anything that can be used as a file
    /// path." This includes `&str`, `String`, `&Path`, and `PathBuf`.
    /// It's how Rust's standard library achieves the same ergonomics
    /// as Python's `os.path` accepting both strings and Path objects.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> Result<(), LedgerError> {
        let path = path.as_ref();

        // Create parent directories if they don't exist (mkdir -p).
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Serialize to pretty JSON for human readability.
        // In production you might use compact JSON for size.
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| LedgerError::Serialization(e.to_string()))?;

        fs::write(path, json)?;
        Ok(())
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
