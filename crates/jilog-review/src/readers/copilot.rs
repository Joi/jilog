//! CopilotReader — scans `~/.copilot/session-state/*/events.jsonl`.
//!
//! GitHub Copilot CLI writes one session-state directory per session,
//! each containing an `events.jsonl` file of event-typed JSON lines:
//!
//!   `{type: "user.message",      data: {role: "user",      content: "..."}}`
//!   `{type: "assistant.message", data: {role: "assistant", content: "..."}}`
//!   `{type: "session.start" | "session.shutdown" | ...}`  (skipped)
//!
//! Only the `user.message` and `assistant.message` events carry chat
//! content; everything else is metadata and is silently skipped.
//!
//! Session ID = the parent directory basename (the per-session UUID).

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};
use crate::util::expand_tilde;

/// Reader for GitHub Copilot CLI sessions.
pub struct CopilotReader {
    pub session_state_dir: PathBuf,
}

impl CopilotReader {
    pub fn new(session_state_dir: impl Into<PathBuf>) -> Self {
        Self { session_state_dir: session_state_dir.into() }
    }

    /// Use the default Copilot session-state directory:
    /// `~/.copilot/session-state`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.copilot/session-state"))
    }
}

impl Reader for CopilotReader {
    fn name(&self) -> &str {
        "copilot"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        if !self.session_state_dir.exists() {
            return Ok(handles);
        }

        let pattern = format!("{}/*/events.jsonl", self.session_state_dir.display());
        let entries = match glob::glob(&pattern) {
            Ok(e) => e,
            Err(err) => {
                return Err(JilogReviewError::Reader(format!(
                    "copilot: glob error: {}",
                    err
                )));
            }
        };

        for entry in entries.flatten() {
            if entry.is_dir() {
                continue;
            }
            let session_dir = match entry.parent() {
                Some(p) => p.to_path_buf(),
                None => continue,
            };
            let session_id = match session_dir.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
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
                reader_name: self.name().to_string(),
                persona: None,
                channel: None,
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
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let (role, want) = match event_type {
                "user.message" => ("user", true),
                "assistant.message" => ("assistant", true),
                // system.message is the static prompt boilerplate — skip;
                // every other type is non-chat session metadata.
                _ => ("", false),
            };
            if !want {
                continue;
            }
            let data = match value.get("data") {
                Some(d) => d,
                None => continue,
            };
            let text = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            out.push(Message {
                role: Some(role.to_string()),
                content: Some(serde_json::Value::String(text.to_string())),
                name: None,
            });
        }
        Ok(out)
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
        let dir = std::env::temp_dir()
            .join("jilog-test-copilot")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copilot_reader_discovers_session_state() {
        let root = test_dir("discover");
        let sess = root.join("00000000-0000-4000-8000-000000000001");
        fs::create_dir_all(&sess).unwrap();
        fs::write(sess.join("events.jsonl"), "").unwrap();
        // Other files in the session dir must NOT be picked up.
        fs::write(sess.join("workspace.yaml"), "").unwrap();
        fs::write(sess.join("session.db"), "").unwrap();

        let reader = CopilotReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].session_id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(handles[0].reader_name, "copilot");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copilot_reader_keeps_user_assistant_skips_session_meta() {
        let root = test_dir("messages");
        let sess = root.join("abc");
        fs::create_dir_all(&sess).unwrap();
        let body = r#"{"type":"session.start","data":{},"id":"1","timestamp":"t","parentId":null}
{"type":"system.message","data":{"role":"system","content":"static prompt"},"id":"2"}
{"type":"user.message","data":{"role":"user","content":"hello copilot"},"id":"3"}
{"type":"assistant.turn_start","data":{},"id":"4"}
{"type":"assistant.message","data":{"role":"assistant","content":"hi human","model":"gpt-x"},"id":"5"}
{"type":"assistant.turn_end","data":{},"id":"6"}
{"type":"session.shutdown","data":{},"id":"7"}
"#;
        fs::write(sess.join("events.jsonl"), body).unwrap();

        let reader = CopilotReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);

        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[0].content.as_ref().and_then(|c| c.as_str()), Some("hello copilot"));
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        let _ = fs::remove_dir_all(&root);
    }
}
