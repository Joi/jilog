//! GenericReader — accepts a configured glob pattern + session-id strategy.
//!
//! Optional per-file header: if the FIRST line of a transcript is
//! `{"_jilog": {"persona": "...", "channel": "..."}}`, the handle carries
//! those dimensions (chat-tuned correction detector + Personas rollup),
//! exactly like the nanoclaw reader stamps them from a cell's routing db.
//! The header has no `role`, so `load()` skips it and older jilog versions
//! ignore it. Exporters that write this format: cell-fleet
//! `scripts/hermes-jilog-export.py` (Hermes profiles on jibotmac).

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

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

/// Persona/channel from an optional `{"_jilog": {...}}` first line.
/// Absent, unparseable, or non-header first lines yield `(None, None)`;
/// this never fails discovery. Only the first line is read (bounded).
pub fn read_header_dims(path: &Path) -> (Option<String>, Option<String>) {
    const MAX_HEADER_BYTES: u64 = 64 * 1024;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (None, None),
    };
    let mut first = String::new();
    if BufReader::new(file.take(MAX_HEADER_BYTES)).read_line(&mut first).is_err() {
        return (None, None);
    }
    let v: serde_json::Value = match serde_json::from_str(first.trim()) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let meta = match v.get("_jilog") {
        Some(serde_json::Value::Object(m)) => m,
        _ => return (None, None),
    };
    let field = |k: &str| {
        meta.get(k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    (field("persona"), field("channel"))
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

            let (persona, channel) = read_header_dims(&entry);

            handles.push(TranscriptHandle {
                session_id,
                path: entry,
                modified,
                reader_name: self.name.clone(),
                persona,
                channel,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("jilog-test-generic").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn header_line_stamps_persona_and_channel_and_is_not_a_message() {
        let dir = test_dir("header");
        let f = dir.join("sess-1.2026-08-17.jsonl");
        fs::write(
            &f,
            concat!(
                "{\"_jilog\":{\"v\":1,\"source\":\"hermes\",\"persona\":\"hermes-line\",\"channel\":\"line\"}}\n",
                "{\"role\":\"user\",\"content\":\"hi\",\"ts\":\"2026-08-17T00:00:01Z\"}\n",
                "{\"role\":\"assistant\",\"content\":\"hello\"}\n",
            ),
        )
        .unwrap();
        let reader = GenericReader::new(
            "hermes",
            format!("{}/*.jsonl", dir.display()),
            SessionIdSource::FileStem,
        );
        let handles = reader.discover(Utc.timestamp_opt(0, 0).single().unwrap()).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].session_id, "sess-1.2026-08-17");
        assert_eq!(handles[0].persona.as_deref(), Some("hermes-line"));
        assert_eq!(handles[0].channel.as_deref(), Some("line"));
        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2, "header line must not be loaded as a message");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_without_a_header_behave_as_before() {
        let dir = test_dir("no-header");
        fs::write(
            dir.join("a.jsonl"),
            "{\"role\":\"user\",\"content\":\"hi\"}\n{\"role\":\"assistant\",\"content\":\"x\"}\n",
        )
        .unwrap();
        // Garbage first line, empty file, header without dims: all (None, None).
        fs::write(dir.join("b.jsonl"), "not json\n{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        fs::write(dir.join("c.jsonl"), "").unwrap();
        fs::write(dir.join("d.jsonl"), "{\"_jilog\":{\"v\":1,\"persona\":\"  \"}}\n").unwrap();
        let reader = GenericReader::new(
            "byo",
            format!("{}/*.jsonl", dir.display()),
            SessionIdSource::FileStem,
        );
        let handles = reader.discover(Utc.timestamp_opt(0, 0).single().unwrap()).unwrap();
        assert_eq!(handles.len(), 4);
        for h in &handles {
            assert!(h.persona.is_none(), "{}", h.session_id);
            assert!(h.channel.is_none(), "{}", h.session_id);
        }
        assert_eq!(reader.load(&handles[0]).unwrap().len(), 2);
        assert_eq!(reader.load(&handles[1]).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
