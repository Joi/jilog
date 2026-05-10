//! Reader trait — pluggable session-log reader abstraction.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::error::JilogReviewError;

// ---------------------------------------------------------------------------
// Message — Schema-B chat message (Anthropic-style content blocks)
// ---------------------------------------------------------------------------

/// A single message from a transcript.jsonl line (Schema-B format).
/// Port from opsctl/crates/opsctl/src/review_nightly.rs verbatim.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Message {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// TranscriptHandle — a discovered transcript file
// ---------------------------------------------------------------------------

/// A discovered transcript file, returned by a Reader's discover() method.
#[derive(Debug, Clone)]
pub struct TranscriptHandle {
    /// Stable session identifier (reader-specific convention).
    pub session_id: String,
    /// Absolute path to the transcript file.
    pub path: PathBuf,
    /// Last-modified timestamp.
    pub modified: DateTime<Utc>,
    /// The reader that produced this handle (e.g. "amplifier", "claude-code").
    pub reader_name: String,
}

// ---------------------------------------------------------------------------
// Reader trait
// ---------------------------------------------------------------------------

/// Pluggable session-log reader.
///
/// Implementations discover transcript files and parse them into [`Message`]
/// slices. The two built-in readers are [`readers::AmplifierReader`] and
/// [`readers::ClaudeCodeReader`].
pub trait Reader: Send + Sync {
    /// Stable name used in digest output and logs (e.g. "amplifier").
    fn name(&self) -> &str;

    /// Discover transcript files modified at-or-after `since`.
    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError>;

    /// Parse a single transcript file into messages.
    /// Implementations MUST silently skip unparseable lines.
    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError>;
}

// ---------------------------------------------------------------------------
// ProcessedSessions — dedup file for "session already processed"
// ---------------------------------------------------------------------------

/// Persistent set of session IDs that have already been processed.
///
/// Unlike the opsctl version, `mark()` only updates the in-memory set.
/// Call `save()` to write the full set to disk.
pub struct ProcessedSessions {
    seen: HashSet<String>,
}

impl ProcessedSessions {
    /// Load from `path`, creating an empty set if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self, JilogReviewError> {
        let mut seen = HashSet::new();
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            for line in content.lines() {
                let s = line.trim();
                if !s.is_empty() {
                    seen.insert(s.to_string());
                }
            }
        }
        Ok(Self { seen })
    }

    /// Return true if `session_id` has already been processed.
    pub fn contains(&self, session_id: &str) -> bool {
        self.seen.contains(session_id)
    }

    /// Mark `session_id` as processed (in-memory only; call `save()` to persist).
    pub fn mark(&mut self, session_id: &str) {
        self.seen.insert(session_id.to_string());
    }

    /// Write the full set of session IDs to `path` (sorted, one per line).
    pub fn save(&self, path: &Path) -> Result<(), JilogReviewError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut sorted: Vec<&String> = self.seen.iter().collect();
        sorted.sort();
        let mut content = String::with_capacity(sorted.len() * 32);
        for id in sorted {
            content.push_str(id);
            content.push('\n');
        }
        std::fs::write(path, content)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("jilog-test-reader")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn processed_sessions_persist_across_load() {
        let dir = test_dir("processed");
        let path = dir.join("processed.txt");

        let mut p = ProcessedSessions::load(&path).unwrap();
        assert!(!p.contains("a"));
        p.mark("a");
        p.mark("b");
        assert!(p.contains("a"));
        p.save(&path).unwrap();

        // Re-load: persists.
        let p2 = ProcessedSessions::load(&path).unwrap();
        assert!(p2.contains("a"));
        assert!(p2.contains("b"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn processed_sessions_idempotent_mark() {
        let dir = test_dir("processed-idempotent");
        let path = dir.join("processed.txt");
        let mut p = ProcessedSessions::load(&path).unwrap();
        p.mark("a");
        p.mark("a"); // second call must not duplicate
        p.save(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
