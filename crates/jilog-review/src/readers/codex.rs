//! CodexReader scans main and pooled Codex session roots at discovery time.
//!
//! Codex CLI writes session rollouts under a date-partitioned tree:
//!
//!   `<root>/YYYY/MM/DD/rollout-YYYY-MM-DDTHH-MM-SS-<uuid>.jsonl`
//!
//! Each line is a wrapper of the form `{ts, type, payload}` where `type`
//! is one of `session_meta`, `turn_context`, `event_msg`, or
//! `response_item`. Only `response_item` lines whose `payload.type` is
//! `message` carry chat content; this reader emits Schema-B messages for
//! those, dropping `developer`-role lines (system prompts) and keeping
//! `user` + `assistant` turns.
//!
//! Session ID = the rollout filename stem (the trailing UUID is the
//! Codex turn identifier, but the full stem stays stable and unique).

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};
use crate::util::expand_tilde;

/// Reader for Codex CLI session rollouts.
pub struct CodexReader {
    pub roots: Vec<PathBuf>,
    pub pool_profiles: Option<PathBuf>,
}

impl CodexReader {
    /// Explicit roots replace defaults and do not implicitly scan the pool.
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self::from_roots(vec![sessions_dir.into()])
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            pool_profiles: None,
        }
    }

    pub fn from_default() -> Self {
        Self {
            roots: vec![expand_tilde("~/.codex/sessions")],
            pool_profiles: Some(expand_tilde("~/.codex-pool/profiles")),
        }
    }

    fn scan_roots(&self) -> Result<Vec<PathBuf>, JilogReviewError> {
        let mut roots = self.roots.clone();
        if let Some(pool) = &self.pool_profiles {
            match std::fs::read_dir(pool) {
                Ok(entries) => {
                    for entry in entries {
                        let sessions = entry?.path().join("sessions");
                        if sessions.is_dir() {
                            roots.push(sessions);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }
}

impl Reader for CodexReader {
    fn name(&self) -> &str {
        "codex"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        let mut seen = std::collections::HashSet::new();
        for root in self.scan_roots()? {
            if !root.is_dir() {
                continue;
            }
            let pattern = format!(
                "{}/**/rollout-*.jsonl",
                glob::Pattern::escape(&root.to_string_lossy())
            );
            let entries = match glob::glob(&pattern) {
                Ok(e) => e,
                Err(err) => {
                    return Err(JilogReviewError::Reader(format!(
                        "codex: glob error: {}",
                        err
                    )));
                }
            };

            for entry in entries.flatten() {
                if entry.is_dir() || !seen.insert(std::fs::canonicalize(&entry)?) {
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
                        Utc.timestamp_opt(secs as i64, 0)
                            .single()
                            .unwrap_or(Utc::now())
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
        }

        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn seat(&self, handle: &TranscriptHandle) -> Option<String> {
        // Use the sessions root, not a date directory or rollout filename.
        handle
            .path
            .ancestors()
            .find(|p| p.file_name().is_some_and(|n| n == "sessions"))
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|n| if n == ".codex" { "main" } else { n }.to_string())
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
            if value.get("type").and_then(|v| v.as_str()) != Some("response_item") {
                continue;
            }
            let payload = match value.get("payload") {
                Some(p) => p,
                None => continue,
            };
            if payload.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            let role = match payload.get("role").and_then(|v| v.as_str()) {
                Some(r) => r,
                None => continue,
            };
            // Skip the system/developer role (instruction blob, not chat).
            if role != "user" && role != "assistant" {
                continue;
            }
            let text = extract_codex_text(payload.get("content"));
            if text.is_empty() {
                continue;
            }
            out.push(Message {
                role: Some(role.to_string()),
                content: Some(serde_json::Value::String(text)),
                name: None,
            });
        }
        Ok(out)
    }
}

/// Pull plain text from a Codex `payload.content` array. Both
/// `input_text` (user) and `output_text` (assistant) content blocks
/// carry a `text` field; everything else is ignored.
fn extract_codex_text(content: Option<&serde_json::Value>) -> String {
    let arr = match content.and_then(|c| c.as_array()) {
        Some(a) => a,
        None => return String::new(),
    };
    let mut parts: Vec<String> = Vec::new();
    for block in arr {
        let t = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(t, "input_text" | "output_text" | "text") {
            if let Some(s) = block.get("text").and_then(|v| v.as_str()) {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::fs;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("jilog-test-codex").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn codex_reader_discovers_dated_rollouts() {
        let root = test_dir("discover");
        let day = root.join("2026").join("03").join("24");
        fs::create_dir_all(&day).unwrap();
        let file =
            day.join("rollout-2026-03-24T09-02-55-00000000-0000-4000-8000-000000000001.jsonl");
        fs::write(&file, "").unwrap();

        let reader = CodexReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert!(handles[0].session_id.starts_with("rollout-2026-03-24"));
        assert_eq!(handles[0].reader_name, "codex");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_reader_keeps_user_and_assistant_skips_developer() {
        let root = test_dir("messages");
        let day = root.join("2026").join("03").join("24");
        fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-03-24T09-02-55-test.jsonl");

        // One developer line (system prompt — must be skipped),
        // one user line, one assistant line, one non-message response_item.
        let body = r#"{"timestamp":"x","type":"session_meta","payload":{}}
{"timestamp":"x","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"system prompt"}]}}
{"timestamp":"x","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi codex"}]}}
{"timestamp":"x","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello back"}]}}
{"timestamp":"x","type":"response_item","payload":{"type":"function_call","name":"x","arguments":"{}"}}
"#;
        fs::write(&file, body).unwrap();

        let reader = CodexReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);

        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(
            msgs[0].content.as_ref().and_then(|c| c.as_str()),
            Some("hi codex")
        );
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn pool_discovery_is_dynamic_and_deduplicates_roots() {
        let tree = tempfile::tempdir().unwrap();
        let main = tree.path().join(".codex/sessions");
        let pool = tree.path().join(".codex-pool/profiles");
        fs::create_dir_all(&main).unwrap();
        fs::write(main.join("rollout-main.jsonl"), "").unwrap();
        let reader = CodexReader {
            roots: vec![main.clone(), main],
            pool_profiles: Some(pool.clone()),
        };
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(reader.seat(&handles[0]).as_deref(), Some("main"));
        for seat in ["codex-01", "codex-02"] {
            let root = pool.join(seat).join("sessions/2026/09/13");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join(format!("rollout-{seat}.jsonl")), "").unwrap();
        }
        fs::create_dir_all(pool.join("unused")).unwrap();
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 3);
        assert!(handles
            .iter()
            .any(|h| reader.seat(h).as_deref() == Some("codex-02")));
        assert_eq!(
            CodexReader::from_roots(vec![])
                .discover(since)
                .unwrap()
                .len(),
            0
        );
        assert!(reader
            .discover(Utc::now() + Duration::days(1))
            .unwrap()
            .is_empty());
    }
}
