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

use rust_decimal::Decimal;

use crate::error::JilogReviewError;
use crate::reader::{
    Message, Reader, SessionEvent, SessionEventKind, SessionStats, TranscriptHandle,
    parse_session_role,
};
use crate::util::{expand_tilde, parse_iso8601};

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

    fn load_events(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<Vec<SessionEvent>>, JilogReviewError> {
        match session_events_path(handle) {
            Some(path) => load_session_events_jsonl(&path).map(Some),
            None => Ok(None),
        }
    }

    fn load_stats(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<SessionStats>, JilogReviewError> {
        match session_events_path(handle) {
            Some(path) => load_session_stats_jsonl(&path, &handle.session_id),
            None => Ok(None),
        }
    }
}

/// The events.jsonl carrying this session's kernel events: the handle's own
/// path when the session was discovered via events.jsonl, or the sibling
/// events.jsonl when it was discovered via transcript.jsonl (discover
/// prefers transcript.jsonl for message fidelity, but kernel events and
/// usage stats only exist in the event log). None when the session has no
/// event log at all — health detectors and the Spend section then get
/// nothing for it.
fn session_events_path(handle: &TranscriptHandle) -> Option<PathBuf> {
    let name = handle.path.file_name().and_then(|n| n.to_str())?;
    if name == "events.jsonl" {
        return Some(handle.path.clone());
    }
    let sibling = handle.path.parent()?.join("events.jsonl");
    if sibling.exists() { Some(sibling) } else { None }
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

/// Parse an Amplifier-format `events.jsonl` into [`SessionEvent`]s for the
/// health detectors. Works for both Amplifier's own event log and the
/// context-intelligence stream (same line format plus an ignored top-level
/// `workspace` key).
///
/// Mapping:
/// - `context:compaction` → `Compaction`
/// - `session:resume`     → `Resume`
/// - `tool:pre`           → `ToolCall` (tool_name = `data.tool_name`,
///                           detail = key-sorted JSON of `data.tool_input`)
/// - `llm:response`       → `LlmResponse`
/// - `prompt:submit`      → `UserMessage`
///
/// `tool:post` is deliberately NOT mapped — the call is counted at `tool:pre`
/// and mapping both would double-count. The timestamp is read from top-level
/// `timestamp` (context-intelligence contract) or `ts` (Amplifier's own log
/// format). Lines that don't parse, name another event, or lack a parseable
/// timestamp are skipped silently: the window-based detectors depend on real
/// timestamps, so a fabricated fallback would produce false storms.
pub(crate) fn load_session_events_jsonl(
    path: &std::path::Path,
) -> Result<Vec<SessionEvent>, JilogReviewError> {
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
        let kind = match value.get("event").and_then(|v| v.as_str()) {
            Some("context:compaction") => SessionEventKind::Compaction,
            Some("session:resume") => SessionEventKind::Resume,
            Some("tool:pre") => SessionEventKind::ToolCall,
            Some("llm:response") => SessionEventKind::LlmResponse,
            Some("prompt:submit") => SessionEventKind::UserMessage,
            _ => continue,
        };
        let timestamp = match value
            .get("timestamp")
            .or_else(|| value.get("ts"))
            .and_then(|v| v.as_str())
            .and_then(parse_iso8601)
        {
            Some(t) => t,
            None => continue,
        };

        let (tool_name, detail) = if kind == SessionEventKind::ToolCall {
            let data = value.get("data");
            let tool_name = data
                .and_then(|d| d.get("tool_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            // serde_json's default Map is a BTreeMap, so to_string() is a
            // key-sorted canonical form — identical arguments compare equal.
            let detail = data
                .and_then(|d| d.get("tool_input"))
                .map(|v| v.to_string());
            (tool_name, detail)
        } else {
            (None, None)
        };

        out.push(SessionEvent { kind, timestamp, tool_name, detail });
    }
    Ok(out)
}

/// Sum usage/cost across the `llm:response` events of an Amplifier-format
/// `events.jsonl` (also used by the context-intelligence reader — same line
/// format).
///
/// Reads `data.usage.{cost_usd,input_tokens,output_tokens}` and attributes
/// cost to `data.model` (falling back to `data.raw.model`). Money math is
/// [`Decimal`] end to end — costs are taken from the JSON literal's
/// shortest-roundtrip text (or a string value verbatim), never summed as
/// floats. Returns `Ok(None)` when no event carried a `usage` object, so
/// sessions without usage data stay out of the digest's Spend section.
pub(crate) fn load_session_stats_jsonl(
    path: &std::path::Path,
    session_id: &str,
) -> Result<Option<SessionStats>, JilogReviewError> {
    let content = std::fs::read_to_string(path)?;
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
        if value.get("event").and_then(|v| v.as_str()) != Some("llm:response") {
            continue;
        }
        let data = match value.get("data") {
            Some(d) => d,
            None => continue,
        };
        let usage = match data.get("usage") {
            Some(u) if u.is_object() => u,
            _ => continue,
        };
        saw_usage = true;
        input_tokens += usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        output_tokens += usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let cost = usage.get("cost_usd").and_then(json_decimal);
        if let Some(cost) = cost {
            total_cost = Some(total_cost.unwrap_or(Decimal::ZERO) + cost);
            let model = data
                .get("model")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("raw").and_then(|r| r.get("model")).and_then(|v| v.as_str()));
            if let Some(model) = model {
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
        role: parse_session_role(session_id),
        model_costs: model_costs
            .into_iter()
            .map(|(m, d)| (m, d.to_string()))
            .collect(),
    }))
}

/// Read a JSON value as a [`Decimal`].
///
/// Numbers go through their shortest-roundtrip text (what `serde_json`
/// prints), which reproduces the upstream literal for any realistic cost
/// value; strings are parsed verbatim. Null, missing, and unparseable
/// values are treated as "no cost".
fn json_decimal(v: &serde_json::Value) -> Option<Decimal> {
    use std::str::FromStr;
    match v {
        serde_json::Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
        serde_json::Value::String(s) => Decimal::from_str(s).ok(),
        _ => None,
    }
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
    fn load_session_events_maps_kinds_and_skips_bad_lines() {
        let dir = test_dir("session-events");
        let path = dir.join("events.jsonl");
        // One of each mapped event, a tool:post (must NOT double-count), an
        // unmapped event, a line without a timestamp, and a garbage line.
        let body = r#"{"event":"session:resume","data":{},"timestamp":"2026-01-01T09:00:00+00:00"}
{"event":"prompt:submit","data":{"prompt":"hi"},"timestamp":"2026-01-01T09:00:01+00:00"}
{"event":"context:compaction","data":{},"timestamp":"2026-01-01T09:00:02+00:00"}
{"event":"tool:pre","data":{"tool_name":"bash","tool_input":{"command":"ls"}},"timestamp":"2026-01-01T09:00:03+00:00"}
{"event":"tool:post","data":{"tool_name":"bash","result":{"success":true}},"timestamp":"2026-01-01T09:00:04+00:00"}
{"event":"llm:response","data":{"raw":{"content":[]}},"timestamp":"2026-01-01T09:00:05+00:00"}
{"event":"session:start","data":{},"timestamp":"2026-01-01T09:00:06+00:00"}
{"event":"tool:pre","data":{"tool_name":"bash","tool_input":{"command":"ls"}}}
{not json
"#;
        fs::write(&path, body).unwrap();

        let events = load_session_events_jsonl(&path).unwrap();
        let kinds: Vec<SessionEventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SessionEventKind::Resume,
                SessionEventKind::UserMessage,
                SessionEventKind::Compaction,
                SessionEventKind::ToolCall,
                SessionEventKind::LlmResponse,
            ]
        );
        let call = &events[3];
        assert_eq!(call.tool_name.as_deref(), Some("bash"));
        assert_eq!(call.detail.as_deref(), Some(r#"{"command":"ls"}"#));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_session_events_accepts_ts_timestamp_key() {
        // Amplifier's own log format uses "ts" (the context-intelligence
        // stream uses "timestamp"); both must parse.
        let dir = test_dir("ts-key");
        let path = dir.join("events.jsonl");
        let body = r#"{"ts": "2026-07-01T09:00:00.123456+00:00", "lvl": "INFO", "event": "tool:pre", "session_id": "s", "data": {"tool_name": "bash", "tool_input": {"command": "ls"}}}
{"ts": "2026-07-01T09:01:00+00:00", "lvl": "INFO", "event": "session:resume", "session_id": "s", "data": {}}
"#;
        fs::write(&path, body).unwrap();
        let events = load_session_events_jsonl(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, SessionEventKind::ToolCall);
        assert_eq!(events[1].kind, SessionEventKind::Resume);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_session_events_detail_is_key_sorted_canonical() {
        let dir = test_dir("canonical-detail");
        let path = dir.join("events.jsonl");
        // Same arguments, different key order on the wire: identical detail.
        let body = r#"{"event":"tool:pre","data":{"tool_name":"bash","tool_input":{"b":2,"a":1}},"timestamp":"2026-01-01T09:00:00+00:00"}
{"event":"tool:pre","data":{"tool_name":"bash","tool_input":{"a":1,"b":2}},"timestamp":"2026-01-01T09:00:01+00:00"}
"#;
        fs::write(&path, body).unwrap();
        let events = load_session_events_jsonl(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].detail, events[1].detail);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_uses_sibling_events_for_transcript_handles() {
        // A session with BOTH files is discovered via transcript.jsonl
        // (message fidelity) but must still yield events and stats from the
        // sibling events.jsonl.
        let root = test_dir("sibling-events");
        let s = root.join("proj").join("sessions").join("sess-both");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("transcript.jsonl"), r#"{"role":"user","content":"hi"}"#).unwrap();
        fs::write(
            s.join("events.jsonl"),
            r#"{"event":"session:resume","data":{},"timestamp":"2026-01-01T09:00:00+00:00"}
{"event":"llm:response","data":{"model":"m","usage":{"cost_usd":0.5,"input_tokens":10,"output_tokens":2}},"timestamp":"2026-01-01T09:00:01+00:00"}
"#,
        )
        .unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert!(handles[0].path.ends_with("transcript.jsonl"));

        let events = reader.load_events(&handles[0]).unwrap().expect("sibling events");
        assert_eq!(events.len(), 2);
        let stats = reader.load_stats(&handles[0]).unwrap().expect("sibling stats");
        assert_eq!(stats.cost_usd.as_deref(), Some("0.5"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reader_load_events_none_for_transcript_some_for_events() {
        let root = test_dir("load-events-gate");
        let s1 = root.join("proj").join("sessions").join("sess-t");
        fs::create_dir_all(&s1).unwrap();
        fs::write(s1.join("transcript.jsonl"), r#"{"role":"user","content":"hi"}"#).unwrap();
        let s2 = root.join("proj").join("sessions").join("sess-e");
        fs::create_dir_all(&s2).unwrap();
        fs::write(
            s2.join("events.jsonl"),
            r#"{"event":"session:resume","data":{},"timestamp":"2026-01-01T09:00:00+00:00"}"#,
        )
        .unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 2);
        for h in &handles {
            let events = reader.load_events(h).unwrap();
            if h.session_id == "sess-t" {
                assert!(events.is_none(), "transcript.jsonl has no event stream");
            } else {
                assert_eq!(events.unwrap().len(), 1);
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    // ---------- session stats ----------

    fn write_stats_fixture(name: &str, lines: &str) -> (PathBuf, PathBuf) {
        let dir = test_dir(name);
        let path = dir.join("events.jsonl");
        fs::write(&path, lines).unwrap();
        (dir, path)
    }

    #[test]
    fn stats_decimal_precision_no_float_drift() {
        // Ten 0.1-style values must sum exactly — the classic float trap.
        let mut lines = String::new();
        for _ in 0..10 {
            lines.push_str(
                r#"{"event":"llm:response","data":{"model":"claude-opus-4-8","usage":{"cost_usd":0.1,"input_tokens":100,"output_tokens":10}}}"#,
            );
            lines.push('\n');
        }
        let (dir, path) = write_stats_fixture("decimal-precision", &lines);
        let stats = load_session_stats_jsonl(&path, "sess").unwrap().unwrap();
        assert_eq!(stats.cost_usd.as_deref(), Some("1.0"));
        assert_eq!(stats.input_tokens, 1000);
        assert_eq!(stats.output_tokens, 100);
        assert_eq!(stats.model_costs.get("claude-opus-4-8").map(String::as_str), Some("1.0"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_null_cost_session_has_usage_but_no_cost() {
        // Unpriced model: usage present, cost_usd null → Some(stats) with
        // cost_usd None, tokens still counted.
        let lines = r#"{"event":"llm:response","data":{"model":"claude-fable-5","usage":{"cost_usd":null,"input_tokens":500,"output_tokens":50}}}
"#;
        let (dir, path) = write_stats_fixture("null-cost", lines);
        let stats = load_session_stats_jsonl(&path, "sess").unwrap().unwrap();
        assert_eq!(stats.cost_usd, None);
        assert_eq!(stats.input_tokens, 500);
        assert!(stats.model_costs.is_empty(), "no cost → no model attribution");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_mixed_priced_and_unpriced_models() {
        let lines = r#"{"event":"llm:response","data":{"model":"claude-opus-4-8","usage":{"cost_usd":0.25,"input_tokens":10,"output_tokens":1}}}
{"event":"llm:response","data":{"model":"claude-fable-5","usage":{"cost_usd":null,"input_tokens":20,"output_tokens":2}}}
{"event":"llm:response","data":{"model":"claude-opus-4-8","usage":{"cost_usd":0.05,"input_tokens":30,"output_tokens":3}}}
"#;
        let (dir, path) = write_stats_fixture("mixed-models", lines);
        let stats = load_session_stats_jsonl(&path, "sess").unwrap().unwrap();
        assert_eq!(stats.cost_usd.as_deref(), Some("0.30"));
        assert_eq!(stats.input_tokens, 60);
        assert_eq!(stats.model_costs.len(), 1, "unpriced model not attributed");
        assert_eq!(stats.model_costs.get("claude-opus-4-8").map(String::as_str), Some("0.30"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_none_when_no_usage_events() {
        let lines = r#"{"event":"prompt:submit","data":{"prompt":"hello"}}
{"event":"llm:response","data":{"raw":{"content":[{"type":"text","text":"hi"}]}}}
"#;
        let (dir, path) = write_stats_fixture("no-usage", lines);
        assert!(load_session_stats_jsonl(&path, "sess").unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_role_parsed_from_session_id_suffix() {
        let lines = r#"{"event":"llm:response","data":{"model":"m","usage":{"cost_usd":1.5,"input_tokens":1,"output_tokens":1}}}
"#;
        let (dir, path) = write_stats_fixture("role-suffix", lines);
        let stats = load_session_stats_jsonl(&path, "0e91a2b4-7d3f_explore").unwrap().unwrap();
        assert_eq!(stats.role.as_deref(), Some("explore"));
        let root = load_session_stats_jsonl(&path, "0e91a2b4-7d3f").unwrap().unwrap();
        assert_eq!(root.role, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_string_cost_and_raw_model_fallback() {
        // cost_usd as a JSON string is taken verbatim; model falls back to
        // data.raw.model when data.model is absent.
        let lines = r#"{"event":"llm:response","data":{"raw":{"model":"claude-haiku-4-5"},"usage":{"cost_usd":"0.0003","input_tokens":1,"output_tokens":1}}}
"#;
        let (dir, path) = write_stats_fixture("string-cost", lines);
        let stats = load_session_stats_jsonl(&path, "sess").unwrap().unwrap();
        assert_eq!(stats.cost_usd.as_deref(), Some("0.0003"));
        assert_eq!(stats.model_costs.get("claude-haiku-4-5").map(String::as_str), Some("0.0003"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_load_stats_none_for_transcript_handles() {
        let root = test_dir("stats-gate");
        let s = root.join("proj").join("sessions").join("sess-t");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("transcript.jsonl"), r#"{"role":"user","content":"hi"}"#).unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert!(reader.load_stats(&handles[0]).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
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
