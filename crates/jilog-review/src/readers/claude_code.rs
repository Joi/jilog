//! ClaudeCodeReader — scans `~/.claude/projects/**/*.jsonl`.
//!
//! Claude Code nests projects by hashed cwd and stores multiple event shapes
//! per file. Only Schema-B-shaped lines (those with `role`/`content`/`name`
//! fields) are kept; everything else is silently skipped.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};
use crate::util::expand_tilde;

/// Reader for Claude Code session transcripts.
///
/// Glob: `<projects_dir>/**/*.jsonl` (recursive).
/// Session ID = filename stem of the .jsonl file.
pub struct ClaudeCodeReader {
    pub projects_dir: PathBuf,
}

impl ClaudeCodeReader {
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self { projects_dir: projects_dir.into() }
    }

    /// Use the default Claude Code projects directory: `~/.claude/projects`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.claude/projects"))
    }
}

impl Reader for ClaudeCodeReader {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        if !self.projects_dir.exists() {
            return Ok(handles);
        }

        // Recursive walk for all .jsonl files.
        let pattern = format!("{}/**/*.jsonl", self.projects_dir.display());
        let entries = match glob::glob(&pattern) {
            Ok(e) => e,
            Err(e) => {
                return Err(JilogReviewError::Reader(format!(
                    "claude-code: glob error: {}",
                    e
                )));
            }
        };

        for entry in entries.flatten() {
            if entry.is_dir() {
                continue;
            }

            let session_id = entry
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| entry.display().to_string());

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
                reader_name: self.name().to_string(),
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
            // Parse to Value first so we can handle both shapes:
            //   legacy flat: {"role": "...", "content": "..."}
            //   wrapped:     {"type": "user", "message": {"role": "...", "content": "..."}}
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(msg) = extract_message(&value) {
                out.push(msg);
            }
        }
        Ok(out)
    }
}

/// Pull a Schema-B `Message` out of a Claude Code JSONL line.
///
/// Real Claude Code lines wrap the chat content under `message`:
///   `{"type":"user","message":{"role":"user","content":...}}`
///   `{"type":"assistant","message":{"role":"assistant","content":[...]}}`
///
/// Legacy/synthetic lines also appear (`{"role":"user","content":"..."}`),
/// so both shapes are accepted. Returns `None` for non-chat lines such as
/// `type=last-prompt`, `permission-mode`, `attachment`, `ai-title`,
/// `file-history-snapshot`.
fn extract_message(value: &serde_json::Value) -> Option<Message> {
    // Wrapped form: inner `message` object carries role/content.
    if let Some(inner) = value.get("message") {
        if inner.get("role").is_some() {
            // Carry the outer "type" forward as a hint when the inner has no name.
            let mut msg: Message = serde_json::from_value(inner.clone()).ok()?;
            if msg.role.is_some() {
                if msg.name.is_none() {
                    if let Some(t) = value.get("type").and_then(|v| v.as_str()) {
                        if t != "user" && t != "assistant" && t != "system" {
                            msg.name = Some(t.to_string());
                        }
                    }
                }
                return Some(msg);
            }
        }
    }

    // Flat form: top-level role/content.
    let msg: Message = serde_json::from_value(value.clone()).ok()?;
    if msg.role.is_some() {
        Some(msg)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use chrono::Duration;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("jilog-test-claude-code")
            .join(name)
    }

    #[test]
    fn claude_code_reader_basic() {
        let root = test_dir("basic");
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("-hash-dir");
        fs::create_dir_all(&proj).unwrap();

        let content = r#"{"role":"user","content":"hello"}
{"someOtherShape": true, "notAMessage": 1}
{"role":"assistant","content":"world"}"#;
        fs::write(proj.join("session-uuid.jsonl"), content).unwrap();

        let reader = ClaudeCodeReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].session_id, "session-uuid");
        assert_eq!(handles[0].reader_name, "claude-code");

        let msgs = reader.load(&handles[0]).unwrap();
        // Only lines with `role` field are kept
        assert_eq!(msgs.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_code_reader_unwraps_message_field() {
        let root = test_dir("wrapped");
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("-Users-joi-repos-x");
        fs::create_dir_all(&proj).unwrap();

        // Real-shape Claude Code lines: type+message wrapper, mixed with
        // session-meta lines that have no role and must be skipped.
        let content = r#"{"type":"last-prompt","leafUuid":"abc","sessionId":"x"}
{"type":"user","uuid":"u1","message":{"role":"user","content":"hello"}}
{"type":"permission-mode","permissionMode":"acceptEdits","sessionId":"x"}
{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}
{"type":"ai-title","title":"t","sessionId":"x"}"#;
        fs::write(proj.join("real-session.jsonl"), content).unwrap();

        let reader = ClaudeCodeReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);

        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2, "user + assistant only — meta lines skipped");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        let _ = fs::remove_dir_all(&root);
    }
}
