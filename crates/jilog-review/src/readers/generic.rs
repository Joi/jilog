//! GenericReader — accepts a configured glob pattern + session-id strategy.

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};

/// Strategy for deriving session IDs from file paths.
pub enum SessionIdSource {
    /// Use the parent directory name as the session ID.
    ParentDir,
    /// Use the file stem (name without extension) as the session ID.
    FileStem,
}

/// A reader configured with an explicit glob pattern and ID strategy.
///
/// Useful for BYO agent systems whose transcript layout doesn't match
/// the built-in readers.
pub struct GenericReader {
    pub name: String,
    /// Glob pattern for transcript files (e.g. `/var/log/agents/*/session-*.jsonl`).
    pub glob_pattern: String,
    pub session_id_from: SessionIdSource,
}

impl GenericReader {
    pub fn new(
        name: impl Into<String>,
        glob_pattern: impl Into<String>,
        session_id_from: SessionIdSource,
    ) -> Self {
        Self {
            name: name.into(),
            glob_pattern: glob_pattern.into(),
            session_id_from,
        }
    }
}

impl Reader for GenericReader {
    fn name(&self) -> &str {
        &self.name
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        let entries = match glob::glob(&self.glob_pattern) {
            Ok(e) => e,
            Err(e) => {
                return Err(JilogReviewError::Reader(format!(
                    "generic reader '{}': glob error: {}",
                    self.name, e
                )));
            }
        };

        for entry in entries.flatten() {
            if entry.is_dir() {
                continue;
            }

            let session_id = match &self.session_id_from {
                SessionIdSource::ParentDir => entry
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| entry.display().to_string()),
                SessionIdSource::FileStem => entry
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| entry.display().to_string()),
            };

            let modified = match entry.metadata().and_then(|m| m.modified()) {
                Ok(st) => {
                    let secs = st
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    Utc.timestamp_opt(secs as i64, 0).single().unwrap_or(Utc::now())
                }
                Err(_) => Utc::now(),
            };

            if modified < since {
                continue;
            }

            handles.push(TranscriptHandle {
                session_id,
                path: entry,
                modified,
                reader_name: self.name.clone(),
            });
        }

        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        let content = std::fs::read_to_string(&handle.path)?;
        let mut out = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(msg) = serde_json::from_str::<Message>(line) {
                if msg.role.is_some() {
                    out.push(msg);
                }
            }
        }
        Ok(out)
    }
}
