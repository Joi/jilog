//! PiReader — scans `~/.pi/agent/sessions/**/*.jsonl`.
//!
//! pi (pi.dev, `@earendil-works/pi-coding-agent`) stores one session per
//! JSONL file under a slugified-cwd directory:
//!
//!   `<sessions>/--<cwd-with-dashes>--/<timestamp>_<uuid>.jsonl`
//!
//! Each line is a session entry with a `type` field (session format v3;
//! see the package's docs/session-format.md). Entries form a tree via
//! `id`/`parentId`, but this reader processes them in file order — for
//! signal detection the linear append order is what happened.
//!
//! Entry types used here:
//! - `message`  → `entry.message` is an AgentMessage; roles `user`,
//!   `assistant`, and `toolResult` are mapped to Schema-B messages.
//!   Content blocks (`text` / `thinking` / `toolCall` / `image`) are
//!   flattened to plain text (text blocks only) so the correction
//!   detector sees real message lengths, not serialized JSON.
//! - `compaction` → health `Compaction` event.
//!
//! `toolResult` messages become `role: "tool"` with
//! `{"success": !isError, "output"|"error": <text>}` so `detect_errors`
//! fires on `isError: true` exactly as it does for Amplifier sessions.
//!
//! Assistant messages carry a `usage` object (tokens + cost in USD) per
//! LLM call; `load_stats` sums those. `input_tokens` counts all
//! input-side tokens (`input + cacheRead + cacheWrite`) since pi reports
//! cache traffic separately. Session ID = the `<uuid>` part of the file
//! stem (after the first `_`); pi has no `<uuid>_<role>` sub-agent
//! convention, so `SessionStats.role` is always None — deriving it from
//! the stem would misread the timestamp/uuid separator as a role suffix.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use rust_decimal::Decimal;

use crate::error::JilogReviewError;
use crate::reader::{
    Message, Reader, SessionEvent, SessionEventKind, SessionStats, TranscriptHandle,
};
use crate::util::{expand_tilde, json_decimal, parse_iso8601};

/// Reader for pi coding-agent session files.
pub struct PiReader {
    pub sessions_dir: PathBuf,
}

impl PiReader {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self { sessions_dir: sessions_dir.into() }
    }

    /// Use the default pi sessions directory: `~/.pi/agent/sessions`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.pi/agent/sessions"))
    }
}

impl Reader for PiReader {
    fn name(&self) -> &str {
        "pi"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        if !self.sessions_dir.exists() {
            return Ok(handles);
        }

        let pattern = format!("{}/**/*.jsonl", self.sessions_dir.display());
        let entries = match glob::glob(&pattern) {
            Ok(e) => e,
            Err(e) => {
                return Err(JilogReviewError::Reader(format!("pi: glob error: {}", e)));
            }
        };

        for entry in entries.flatten() {
            if entry.is_dir() {
                continue;
            }

            let stem = entry
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| entry.display().to_string());
            let session_id = session_id_from_stem(&stem);

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
            if value.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            let msg = match value.get("message") {
                Some(m) => m,
                None => continue,
            };
            match msg.get("role").and_then(|v| v.as_str()) {
                Some("user") => {
                    let text = extract_pi_text(msg.get("content"));
                    if text.is_empty() {
                        continue;
                    }
                    out.push(Message {
                        role: Some("user".to_string()),
                        content: Some(serde_json::Value::String(text)),
                        name: None,
                    });
                }
                Some("assistant") => {
                    let text = extract_pi_text(msg.get("content"));
                    if text.is_empty() {
                        continue;
                    }
                    out.push(Message {
                        role: Some("assistant".to_string()),
                        content: Some(serde_json::Value::String(text)),
                        name: None,
                    });
                }
                Some("toolResult") => {
                    let tool_name = msg
                        .get("toolName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let is_error = msg
                        .get("isError")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let text = extract_pi_text(msg.get("content"));
                    // Preserve the pass/fail shape detect_errors expects on
                    // tool-role lines: success:false + an error payload.
                    let content = if is_error {
                        serde_json::json!({"success": false, "error": text})
                    } else {
                        serde_json::json!({"success": true, "output": text})
                    };
                    out.push(Message {
                        role: Some("tool".to_string()),
                        content: Some(content),
                        name: if tool_name.is_empty() { None } else { Some(tool_name) },
                    });
                }
                // bashExecution (user-typed ! commands), custom,
                // branchSummary, compactionSummary: not chat turns.
                _ => continue,
            }
        }
        Ok(out)
    }

    fn load_events(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<Vec<SessionEvent>>, JilogReviewError> {
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
            let timestamp = match value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(parse_iso8601)
            {
                Some(t) => t,
                None => continue,
            };
            match value.get("type").and_then(|v| v.as_str()) {
                Some("compaction") => {
                    out.push(SessionEvent {
                        kind: SessionEventKind::Compaction,
                        timestamp,
                        tool_name: None,
                        detail: None,
                    });
                }
                Some("message") => {
                    let msg = match value.get("message") {
                        Some(m) => m,
                        None => continue,
                    };
                    match msg.get("role").and_then(|v| v.as_str()) {
                        Some("user") => {
                            out.push(SessionEvent {
                                kind: SessionEventKind::UserMessage,
                                timestamp,
                                tool_name: None,
                                detail: None,
                            });
                        }
                        Some("assistant") => {
                            out.push(SessionEvent {
                                kind: SessionEventKind::LlmResponse,
                                timestamp,
                                tool_name: None,
                                detail: None,
                            });
                            // Tool calls live inside assistant content blocks;
                            // emit one ToolCall per block. serde_json's default
                            // Map is a BTreeMap, so to_string() of the arguments
                            // is a key-sorted canonical form — identical
                            // arguments compare equal as strings.
                            if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                                for block in blocks {
                                    if block.get("type").and_then(|v| v.as_str())
                                        != Some("toolCall")
                                    {
                                        continue;
                                    }
                                    let tool_name = block
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    let detail =
                                        block.get("arguments").map(|v| v.to_string());
                                    out.push(SessionEvent {
                                        kind: SessionEventKind::ToolCall,
                                        timestamp,
                                        tool_name,
                                        detail,
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(Some(out))
    }

    fn load_stats(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<SessionStats>, JilogReviewError> {
        let content = std::fs::read_to_string(&handle.path)?;
        let mut saw_usage = false;
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut total_cost: Option<Decimal> = None;
        let mut model_costs: std::collections::BTreeMap<String, Decimal> =
            std::collections::BTreeMap::new();

        for line in content.lines() {
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if value.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            let msg = match value.get("message") {
                Some(m) => m,
                None => continue,
            };
            if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            let usage = match msg.get("usage") {
                Some(u) if u.is_object() => u,
                _ => continue,
            };
            saw_usage = true;
            // All input-side tokens: pi reports cache reads/writes separately
            // from uncached input.
            for key in ["input", "cacheRead", "cacheWrite"] {
                input_tokens += usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            }
            output_tokens += usage.get("output").and_then(|v| v.as_u64()).unwrap_or(0);

            let cost = usage
                .get("cost")
                .and_then(|c| c.get("total"))
                .and_then(json_decimal);
            if let Some(cost) = cost {
                total_cost = Some(total_cost.unwrap_or(Decimal::ZERO) + cost);
                if let Some(model) = msg.get("model").and_then(|v| v.as_str()) {
                    *model_costs.entry(model.to_string()).or_insert(Decimal::ZERO) += cost;
                }
            }
        }

        if !saw_usage {
            return Ok(None);
        }
        Ok(Some(SessionStats {
            cost_usd: total_cost.map(|d| d.to_string()),
            input_tokens,
            output_tokens,
            // pi has no `<uuid>_<role>` sub-agent convention (see module docs).
            role: None,
            model_costs: model_costs
                .into_iter()
                .map(|(m, d)| (m, d.to_string()))
                .collect(),
        }))
    }
}

/// Session ID from a pi session file stem (`<timestamp>_<uuid>` → `<uuid>`).
/// The timestamp prefix is dropped so the ID matches the header's session
/// UUID; stems without an underscore are used verbatim.
fn session_id_from_stem(stem: &str) -> String {
    match stem.split_once('_') {
        Some((_, uuid)) if !uuid.is_empty() => uuid.to_string(),
        _ => stem.to_string(),
    }
}

/// Flatten pi message content to plain text. Content is either a string
/// (legacy user form) or an array of typed blocks; only `text` blocks
/// contribute (thinking / toolCall / image blocks are dropped).
fn extract_pi_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts: Vec<String> = Vec::new();
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
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
            .join("jilog-test-pi")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A realistic minimal pi v3 session: header, model/thinking meta, a
    /// user turn, an assistant turn with thinking + text + toolCall, a
    /// failing toolResult, and a closing assistant turn.
    const SESSION_BODY: &str = r#"{"type":"session","version":3,"id":"019f3459-6e33-74bd-80eb-7b0c31692283","timestamp":"2026-07-05T22:15:03.987Z","cwd":"/tmp/proj"}
{"type":"model_change","id":"ca048221","parentId":null,"timestamp":"2026-07-05T22:15:03.993Z","provider":"anthropic","modelId":"claude-opus-4-8"}
{"type":"thinking_level_change","id":"e63c9265","parentId":"ca048221","timestamp":"2026-07-05T22:15:03.993Z","thinkingLevel":"medium"}
{"type":"message","id":"8ecb36d1","parentId":"e63c9265","timestamp":"2026-07-05T22:15:03.997Z","message":{"role":"user","content":[{"type":"text","text":"run the thing"}],"timestamp":1783289703996}}
{"type":"message","id":"67bcc6b5","parentId":"8ecb36d1","timestamp":"2026-07-05T22:15:44.953Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"internal"},{"type":"text","text":"Running it as a workaround for now."},{"type":"toolCall","id":"toolu_01","name":"bash","arguments":{"command":"run-thing"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-opus-4-8","usage":{"input":2,"output":66,"cacheRead":100,"cacheWrite":2425,"totalTokens":2593,"cost":{"input":1e-05,"output":0.00165,"cacheRead":0.0001,"cacheWrite":0.0151,"total":0.0169},"reasoning":9},"stopReason":"toolUse","timestamp":1783289742401}}
{"type":"message","id":"8f3de77a","parentId":"67bcc6b5","timestamp":"2026-07-05T22:15:44.962Z","message":{"role":"toolResult","toolCallId":"toolu_01","toolName":"bash","content":[{"type":"text","text":"command not found"}],"isError":true,"timestamp":1783289744962}}
{"type":"message","id":"1faf82cd","parentId":"8f3de77a","timestamp":"2026-07-05T22:15:46.744Z","message":{"role":"assistant","content":[{"type":"text","text":"That failed."}],"api":"anthropic-messages","provider":"anthropic","model":"claude-opus-4-8","usage":{"input":2,"output":19,"cacheRead":2425,"cacheWrite":80,"totalTokens":2526,"cost":{"input":1e-05,"output":0.000475,"cacheRead":0.0012125,"cacheWrite":0.0005,"total":0.0021975},"reasoning":0},"stopReason":"stop","timestamp":1783289744963}}
{"type":"compaction","id":"f6g7h8i9","parentId":"1faf82cd","timestamp":"2026-07-05T22:20:00.000Z","summary":"earlier context","firstKeptEntryId":"8ecb36d1","tokensBefore":50000}
{"type":"custom","id":"h8i9j0k1","parentId":"f6g7h8i9","timestamp":"2026-07-05T22:21:00.000Z","customType":"some-extension","data":{"count":42}}
"#;

    fn write_session(name: &str) -> (PathBuf, PathBuf) {
        let root = test_dir(name);
        let proj = root.join("--tmp-proj--");
        fs::create_dir_all(&proj).unwrap();
        let file = proj.join("2026-07-05T22-15-03-987Z_019f3459-6e33-74bd-80eb-7b0c31692283.jsonl");
        fs::write(&file, SESSION_BODY).unwrap();
        (root, file)
    }

    #[test]
    fn pi_reader_discovers_and_derives_uuid_session_id() {
        let (root, _file) = write_session("discover");

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].session_id, "019f3459-6e33-74bd-80eb-7b0c31692283");
        assert_eq!(handles[0].reader_name, "pi");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn session_id_from_stem_forms() {
        assert_eq!(
            session_id_from_stem("2026-07-05T22-15-03-987Z_019f3459-6e33"),
            "019f3459-6e33"
        );
        assert_eq!(session_id_from_stem("no-underscore"), "no-underscore");
        assert_eq!(session_id_from_stem("trailing_"), "trailing_");
    }

    #[test]
    fn pi_reader_maps_user_assistant_tool_and_skips_meta() {
        let (root, _file) = write_session("messages");

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        let msgs = reader.load(&handles[0]).unwrap();

        assert_eq!(msgs.len(), 4, "user, assistant, tool, assistant");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(
            msgs[0].content.as_ref().and_then(|c| c.as_str()),
            Some("run the thing")
        );
        // Assistant text flattened: thinking + toolCall blocks dropped.
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        assert_eq!(
            msgs[1].content.as_ref().and_then(|c| c.as_str()),
            Some("Running it as a workaround for now.")
        );
        // Failing toolResult → tool role with success:false + error text.
        assert_eq!(msgs[2].role.as_deref(), Some("tool"));
        assert_eq!(msgs[2].name.as_deref(), Some("bash"));
        let tool_content = msgs[2].content.as_ref().unwrap();
        assert_eq!(tool_content.get("success").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            tool_content.get("error").and_then(|v| v.as_str()),
            Some("command not found")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pi_reader_accepts_string_user_content() {
        let root = test_dir("string-content");
        let proj = root.join("--x--");
        fs::create_dir_all(&proj).unwrap();
        let body = r#"{"type":"session","version":3,"id":"u","timestamp":"2026-07-05T22:15:03.987Z","cwd":"/x"}
{"type":"message","id":"a","parentId":null,"timestamp":"2026-07-05T22:15:04.000Z","message":{"role":"user","content":"plain string prompt","timestamp":1}}
"#;
        fs::write(proj.join("2026-07-05T00-00-00-000Z_u.jsonl"), body).unwrap();

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].content.as_ref().and_then(|c| c.as_str()),
            Some("plain string prompt")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pi_reader_events_map_kinds_and_tool_calls() {
        let (root, _file) = write_session("events");

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        let events = reader.load_events(&handles[0]).unwrap().expect("pi always has events");

        let kinds: Vec<SessionEventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SessionEventKind::UserMessage,
                SessionEventKind::LlmResponse,
                SessionEventKind::ToolCall,
                SessionEventKind::LlmResponse,
                SessionEventKind::Compaction,
            ]
        );
        let call = &events[2];
        assert_eq!(call.tool_name.as_deref(), Some("bash"));
        assert_eq!(call.detail.as_deref(), Some(r#"{"command":"run-thing"}"#));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pi_reader_stats_sum_usage_and_costs() {
        let (root, _file) = write_session("stats");

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        let stats = reader.load_stats(&handles[0]).unwrap().expect("usage present");

        // input-side: (2+100+2425) + (2+2425+80); output: 66 + 19.
        assert_eq!(stats.input_tokens, 5034);
        assert_eq!(stats.output_tokens, 85);
        // 0.0169 + 0.0021975, exact decimal — no float drift.
        assert_eq!(stats.cost_usd.as_deref(), Some("0.0190975"));
        assert_eq!(
            stats.model_costs.get("claude-opus-4-8").map(String::as_str),
            Some("0.0190975")
        );
        assert_eq!(stats.role, None, "pi has no sub-agent role suffix convention");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pi_reader_stats_none_without_usage() {
        let root = test_dir("no-usage");
        let proj = root.join("--x--");
        fs::create_dir_all(&proj).unwrap();
        let body = r#"{"type":"session","version":3,"id":"u","timestamp":"2026-07-05T22:15:03.987Z","cwd":"/x"}
{"type":"message","id":"a","parentId":null,"timestamp":"2026-07-05T22:15:04.000Z","message":{"role":"user","content":"hi","timestamp":1}}
"#;
        fs::write(proj.join("2026-07-05T00-00-00-000Z_u.jsonl"), body).unwrap();

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert!(reader.load_stats(&handles[0]).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pi_reader_skips_garbage_and_missing_timestamps() {
        let root = test_dir("garbage");
        let proj = root.join("--x--");
        fs::create_dir_all(&proj).unwrap();
        let body = r#"{"type":"session","version":3,"id":"u","timestamp":"2026-07-05T22:15:03.987Z","cwd":"/x"}
not json at all
{"type":"message","id":"a","parentId":null,"message":{"role":"user","content":"no timestamp","timestamp":1}}
{"type":"message","id":"b","parentId":"a","timestamp":"2026-07-05T22:15:05.000Z","message":{"role":"user","content":"has timestamp","timestamp":2}}
"#;
        fs::write(proj.join("2026-07-05T00-00-00-000Z_u.jsonl"), body).unwrap();

        let reader = PiReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();

        // load keeps both user messages (no timestamp needed for messages)…
        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2);
        // …but events require a parseable top-level timestamp.
        let events = reader.load_events(&handles[0]).unwrap().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, SessionEventKind::UserMessage);
        let _ = fs::remove_dir_all(&root);
    }
}
