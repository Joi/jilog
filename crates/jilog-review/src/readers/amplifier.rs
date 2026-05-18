//! AmplifierReader — scans Amplifier session logs.
//!
//! Amplifier's on-disk layout has evolved:
//!
//!   legacy:   `<projects>/<sess>/transcript.jsonl`
//!   current:  `<projects>/<project>/sessions/<sess>/transcript.jsonl`
//!   current:  `<projects>/<project>/sessions/<sess>/events.jsonl`  (newer)
//!
//! This reader discovers all three. `transcript.jsonl` carries Schema-B
//! chat messages directly. `events.jsonl` is a structured event log; the
//! loader synthesizes Schema-B messages from `prompt:submit` (user),
//! `llm:response` (assistant text), and `tool:post` (tool results) events
//! so the downstream detectors can run against either format.
//!
//! Session ID = the parent directory basename of the discovered file.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};
use crate::util::expand_tilde;

/// Reader for Amplifier-style session transcripts.
pub struct AmplifierReader {
    pub projects_dir: PathBuf,
}

impl AmplifierReader {
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self { projects_dir: projects_dir.into() }
    }

    /// Use the default Amplifier projects directory: `~/.amplifier/projects`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.amplifier/projects"))
    }
}

impl Reader for AmplifierReader {
    fn name(&self) -> &str {
        "amplifier"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        if !self.projects_dir.exists() {
            return Ok(handles);
        }

        // Three glob patterns cover legacy + current Amplifier layouts.
        // If a session has BOTH transcript.jsonl and events.jsonl, prefer
        // transcript.jsonl (it's the raw Schema-B form; cheaper to parse).
        let base = self.projects_dir.display();
        let patterns = [
            format!("{}/*/transcript.jsonl", base),                  // legacy
            format!("{}/*/sessions/*/transcript.jsonl", base),       // current, with transcript
            format!("{}/*/sessions/*/events.jsonl", base),           // current, events-only
        ];

        // Track which session dirs we've already covered so we don't add
        // events.jsonl for a session whose transcript.jsonl we already added.
        use std::collections::HashSet;
        let mut covered_session_dirs: HashSet<PathBuf> = HashSet::new();

        for pat in &patterns {
            let entries = match glob::glob(pat) {
                Ok(e) => e,
                Err(err) => {
                    return Err(JilogReviewError::Reader(format!(
                        "amplifier: glob error for {}: {}",
                        pat, err
                    )));
                }
            };

            for entry in entries.flatten() {
                let session_dir = match entry.parent() {
                    Some(p) => p.to_path_buf(),
                    None => continue,
                };
                if covered_session_dirs.contains(&session_dir) {
                    continue;
                }

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

                covered_session_dirs.insert(session_dir);
                handles.push(TranscriptHandle {
                    session_id,
                    path: entry,
                    modified,
                    reader_name: self.name().to_string(),
                });
            }
        }

        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        let is_events = handle
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s == "events.jsonl")
            .unwrap_or(false);

        if is_events {
            load_events_jsonl(&handle.path)
        } else {
            load_transcript_jsonl(&handle.path)
        }
    }
}

/// Parse a `transcript.jsonl` file (Schema-B; one chat message per line).
/// Invalid lines are skipped silently.
pub(crate) fn load_transcript_jsonl(path: &std::path::Path) -> Result<Vec<Message>, JilogReviewError> {
    let content = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<Message>(line) {
            out.push(msg);
        }
    }
    Ok(out)
}

/// Parse an Amplifier `events.jsonl` structured-log file and synthesize
/// Schema-B messages from the chat-relevant events.
///
/// Mapping:
/// - `prompt:submit`   → `{role: "user",      content: data.prompt}`
/// - `llm:response`    → `{role: "assistant", content: <extracted text>}`
/// - `tool:post`       → `{role: "tool",      name:    data.tool_name,
///                          content: JSON({success, output|error})}`
///
/// The tool message preserves `success` and the structured `output` so
/// the existing `detect_errors` detector (which looks for
/// `success: false` on tool-role lines) fires the same way it would on a
/// transcript.jsonl.
pub(crate) fn load_events_jsonl(path: &std::path::Path) -> Result<Vec<Message>, JilogReviewError> {
    let content = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event = value.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let data = value.get("data");
        match event {
            "prompt:submit" => {
                if let Some(prompt) = data
                    .and_then(|d| d.get("prompt"))
                    .and_then(|p| p.as_str())
                {
                    out.push(Message {
                        role: Some("user".to_string()),
                        content: Some(serde_json::Value::String(prompt.to_string())),
                        name: None,
                    });
                }
            }
            "llm:response" => {
                if let Some(text) = extract_assistant_text(data) {
                    if !text.is_empty() {
                        out.push(Message {
                            role: Some("assistant".to_string()),
                            content: Some(serde_json::Value::String(text)),
                            name: None,
                        });
                    }
                }
            }
            "tool:post" => {
                if let Some(d) = data {
                    let tool_name = d
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Preserve the structured result so detect_errors can see
                    // `success: false` on the tool-role line.
                    let result = d.get("result").cloned().unwrap_or(serde_json::Value::Null);
                    out.push(Message {
                        role: Some("tool".to_string()),
                        content: Some(result),
                        name: if tool_name.is_empty() { None } else { Some(tool_name) },
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Pull a plain-text representation of an assistant turn from an
/// `llm:response` event. Concatenates all `text`-type content blocks in
/// `data.raw.content`; ignores tool-use, thinking, and other non-text
/// blocks (workaround/deferral detectors only look at plain text anyway).
fn extract_assistant_text(data: Option<&serde_json::Value>) -> Option<String> {
    let raw = data?.get("raw")?;
    let content = raw.get("content")?.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                parts.push(t.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
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
            .join("jilog-test-amplifier")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discover_finds_legacy_flat_layout() {
        let root = test_dir("legacy-flat");
        let s1 = root.join("session-aaaa");
        fs::create_dir_all(&s1).unwrap();
        fs::write(s1.join("transcript.jsonl"), "").unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "session-aaaa");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_finds_current_nested_layout() {
        let root = test_dir("current-nested");
        let s = root.join("-tmp-proj").join("sessions").join("abc-123");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("transcript.jsonl"), "").unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "abc-123");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_picks_up_events_only_sessions() {
        let root = test_dir("events-only");
        let s = root.join("proj").join("sessions").join("sess-xyz");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("events.jsonl"), "").unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "sess-xyz");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_prefers_transcript_over_events_when_both_present() {
        let root = test_dir("both");
        let s = root.join("proj").join("sessions").join("sess-both");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("transcript.jsonl"), "").unwrap();
        fs::write(s.join("events.jsonl"), "").unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("transcript.jsonl"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_transcript_skips_blank_and_invalid() {
        let dir = test_dir("load-transcript");
        let path = dir.join("transcript.jsonl");
        fs::write(
            &path,
            r#"{"role":"user","content":"hello world"}

not json
{"role":"assistant","content":"hi"}
{"unclosed":"json
"#,
        )
        .unwrap();

        let msgs = load_transcript_jsonl(&path).unwrap();
        assert_eq!(msgs.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_events_synthesizes_user_assistant_tool() {
        let dir = test_dir("load-events");
        let path = dir.join("events.jsonl");
        // Minimal but realistic Amplifier event log: one user prompt, one
        // assistant text reply, one failing tool call.
        let body = r#"{"event":"session:start","data":{}}
{"event":"prompt:submit","data":{"prompt":"hello"}}
{"event":"llm:response","data":{"raw":{"content":[{"type":"text","text":"hi there"}]}}}
{"event":"tool:pre","data":{"tool_name":"bash","tool_input":{"command":"x"}}}
{"event":"tool:post","data":{"tool_name":"bash","result":{"success":false,"error":"boom"}}}
{"event":"session:end","data":{}}
"#;
        fs::write(&path, body).unwrap();

        let msgs = load_events_jsonl(&path).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        assert_eq!(msgs[2].role.as_deref(), Some("tool"));
        assert_eq!(msgs[2].name.as_deref(), Some("bash"));
        // Tool content preserves success:false so detect_errors works.
        let tool_content = msgs[2].content.as_ref().unwrap();
        assert_eq!(tool_content.get("success").and_then(|v| v.as_bool()), Some(false));
        let _ = fs::remove_dir_all(&dir);
    }
}
