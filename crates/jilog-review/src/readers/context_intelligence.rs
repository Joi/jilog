//! ContextIntelligenceReader — scans Amplifier context-intelligence event logs.
//!
//! The `amplifier-bundle-context-intelligence` hook writes a versioned
//! per-session event stream alongside the regular Amplifier session files:
//!
//!   `<projects>/<project>/sessions/<sess>/context-intelligence/events.jsonl`
//!   `<projects>/<project>/sessions/<sess>/context-intelligence/metadata.json`
//!
//! Each `events.jsonl` line is the raw kernel event triple plus an injected
//! `workspace` key, all with sorted keys:
//!
//!   `{data: {<payload>}, event: "<name>", timestamp: "<ISO-8601>", workspace: "<id>"}`
//!
//! Payloads live entirely under `data` — no fields are promoted to top level —
//! so the line format is a superset of Amplifier's own `events.jsonl` and the
//! same Schema-B synthesis applies (`prompt:submit` → user, `llm:response` →
//! assistant text, `tool:post` → tool result). `load()` delegates to the
//! amplifier reader's event parser; the extra `workspace` key is ignored by
//! that parser. Note the `Signal` type carries only `session_id` as
//! provenance, so `workspace` is validated but not propagated downstream.
//!
//! The sibling `metadata.json` carries a schema contract that MANDATES a
//! fail-loud version check before reading event lines: `format` must be
//! `"context-intelligence"` and the semver major must be 1. Sessions that
//! fail the check are skipped with a warning — loud per contract, but one bad
//! session must not abort the whole review run. `metadata.json` also carries
//! `last_event_at`, which is a more reliable recency source than file mtime
//! (the hook updates it after every event append), so discovery prefers it
//! for the `since` filter.
//!
//! Session ID = the `sessions/<sess>` directory basename (per the contract,
//! that path component IS the session id).

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, SessionEvent, SessionStats, TranscriptHandle};
use crate::util::{expand_tilde, parse_iso8601};

use super::amplifier::{load_events_jsonl, load_session_events_jsonl, load_session_stats_jsonl};

/// Reader for Amplifier context-intelligence session event logs.
pub struct ContextIntelligenceReader {
    pub projects_dir: PathBuf,
}

impl ContextIntelligenceReader {
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self { projects_dir: projects_dir.into() }
    }

    /// Use the default Amplifier projects directory: `~/.amplifier/projects`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.amplifier/projects"))
    }
}

impl Reader for ContextIntelligenceReader {
    fn name(&self) -> &str {
        "context-intelligence"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        if !self.projects_dir.exists() {
            return Ok(handles);
        }

        let pattern = format!(
            "{}/*/sessions/*/context-intelligence/events.jsonl",
            self.projects_dir.display()
        );
        let entries = match glob::glob(&pattern) {
            Ok(e) => e,
            Err(err) => {
                return Err(JilogReviewError::Reader(format!(
                    "context-intelligence: glob error: {}",
                    err
                )));
            }
        };

        for entry in entries.flatten() {
            if entry.is_dir() {
                continue;
            }
            // entry = .../sessions/<sess>/context-intelligence/events.jsonl
            let ci_dir = match entry.parent() {
                Some(p) => p.to_path_buf(),
                None => continue,
            };
            let session_dir = match ci_dir.parent() {
                Some(p) => p.to_path_buf(),
                None => continue,
            };
            let session_id = match session_dir.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            // MANDATED version gate: check metadata.json before the session's
            // event lines are ever parsed. Fail loud (warn) but keep the run
            // alive — one incompatible session must not sink the review.
            let meta = match check_session_metadata(&ci_dir) {
                Ok(m) => m,
                Err(reason) => {
                    tracing::warn!(
                        "context-intelligence: skipping session {} ({}): {}",
                        session_id,
                        ci_dir.display(),
                        reason
                    );
                    continue;
                }
            };

            // Prefer metadata's last_event_at (updated after every append)
            // over file mtime; fall back to mtime when absent/unparseable.
            let modified = meta
                .last_event_at
                .as_deref()
                .and_then(parse_iso8601)
                .unwrap_or_else(|| match entry.metadata().and_then(|m| m.modified()) {
                    Ok(st) => {
                        let secs = st
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        Utc.timestamp_opt(secs as i64, 0).single().unwrap_or(Utc::now())
                    }
                    Err(_) => Utc::now(),
                });

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
        // The context-intelligence line format is the amplifier event triple
        // plus an injected top-level `workspace` key, which the amplifier
        // parser ignores — same event names, same payload shapes under `data`.
        load_events_jsonl(&handle.path)
    }

    fn load_events(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<Vec<SessionEvent>>, JilogReviewError> {
        // Every discovered handle is an events.jsonl (that is the only file
        // this reader globs), so the richer stream is always available.
        load_session_events_jsonl(&handle.path).map(Some)
    }

    fn load_stats(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<SessionStats>, JilogReviewError> {
        load_session_stats_jsonl(&handle.path, &handle.session_id)
    }
}

// ---------------------------------------------------------------------------
// metadata.json contract check
// ---------------------------------------------------------------------------

/// The subset of `metadata.json` fields the reader consumes.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct CiMetadata {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// Most recent event timestamp; updated after every event append.
    #[serde(default)]
    pub last_event_at: Option<String>,
}

/// Read and validate `<ci_dir>/metadata.json` per the schema contract:
/// `format` must be `"context-intelligence"` and the version's semver major
/// must be 1 (any 1.x is accepted). Returns the parsed metadata on success,
/// or a human-readable skip reason on any violation — including a missing or
/// malformed metadata.json, since reading events without the version gate is
/// exactly what the contract forbids.
pub(crate) fn check_session_metadata(ci_dir: &Path) -> Result<CiMetadata, String> {
    let meta_path = ci_dir.join("metadata.json");
    let raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("metadata.json unreadable: {}", e))?;
    let meta: CiMetadata = serde_json::from_str(&raw)
        .map_err(|e| format!("metadata.json malformed: {}", e))?;

    match meta.format.as_deref() {
        Some("context-intelligence") => {}
        other => return Err(format!("unexpected format: {:?}", other)),
    }

    let version = meta
        .version
        .as_deref()
        .ok_or_else(|| "metadata.json has no version".to_string())?;
    let major = version.split('.').next().unwrap_or("");
    if major != "1" {
        return Err(format!("unsupported version: {:?} (expected 1.x)", version));
    }

    Ok(meta)
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
            .join("jilog-test-context-intelligence")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create `<root>/proj/sessions/<sess>/context-intelligence/` with the
    /// given metadata.json body (None = no metadata.json) and an empty
    /// events.jsonl. Returns the context-intelligence dir.
    fn make_session(root: &Path, sess: &str, metadata: Option<&str>) -> PathBuf {
        let ci = root
            .join("proj")
            .join("sessions")
            .join(sess)
            .join("context-intelligence");
        fs::create_dir_all(&ci).unwrap();
        fs::write(ci.join("events.jsonl"), "").unwrap();
        if let Some(body) = metadata {
            fs::write(ci.join("metadata.json"), body).unwrap();
        }
        ci
    }

    const VALID_META: &str = r#"{"format":"context-intelligence","version":"1.0.0","session_id":"s","workspace":"w","started_at":"2026-01-01T00:00:00+00:00","status":"running"}"#;

    #[test]
    fn discover_finds_valid_session() {
        let root = test_dir("valid");
        make_session(&root, "sess-aaa", Some(VALID_META));

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "sess-aaa");
        assert_eq!(found[0].reader_name, "context-intelligence");
        assert!(found[0].path.ends_with("context-intelligence/events.jsonl"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_accepts_any_one_x_version() {
        let root = test_dir("one-x");
        make_session(
            &root,
            "sess-1x",
            Some(r#"{"format":"context-intelligence","version":"1.4.2"}"#),
        );

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        assert_eq!(reader.discover(since).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_skips_wrong_version_and_wrong_format() {
        let root = test_dir("wrong-meta");
        make_session(
            &root,
            "sess-v2",
            Some(r#"{"format":"context-intelligence","version":"2.0.0"}"#),
        );
        make_session(
            &root,
            "sess-fmt",
            Some(r#"{"format":"something-else","version":"1.0.0"}"#),
        );
        // A valid session alongside them must still come through.
        make_session(&root, "sess-ok", Some(VALID_META));

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "sess-ok");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_skips_missing_or_malformed_metadata() {
        let root = test_dir("bad-meta");
        make_session(&root, "sess-none", None);
        make_session(&root, "sess-garbled", Some("{not json"));

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        assert_eq!(reader.discover(since).unwrap().len(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_uses_last_event_at_for_since_filter() {
        let root = test_dir("last-event-at");
        // File mtime is "now", but metadata says the last event was in 2020 —
        // the session must be filtered out by a recent `since`.
        make_session(
            &root,
            "sess-stale",
            Some(
                r#"{"format":"context-intelligence","version":"1.0.0","last_event_at":"2020-01-01T00:00:00+00:00"}"#,
            ),
        );

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        assert_eq!(reader.discover(since).unwrap().len(), 0);

        // With an early-enough `since` the same session is picked up, and its
        // modified timestamp reflects last_event_at, not mtime.
        let epoch = Utc.timestamp_opt(0, 0).single().unwrap();
        let found = reader.discover(epoch).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].modified.format("%Y").to_string(), "2020");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_synthesizes_messages_from_envelope_lines() {
        let root = test_dir("load");
        let ci = make_session(&root, "sess-load", Some(VALID_META));
        // Realistic context-intelligence lines: sorted keys, payload under
        // `data`, injected `workspace`, plus one malformed line to skip.
        let body = r#"{"data":{"session_id":"s"},"event":"session:start","timestamp":"2026-01-01T00:00:00+00:00","workspace":"w"}
{"data":{"prompt":"hello","turn":1},"event":"prompt:submit","timestamp":"2026-01-01T00:00:01+00:00","workspace":"w"}
{"data":{"raw":{"content":[{"text":"hi there","type":"text"}]}},"event":"llm:response","timestamp":"2026-01-01T00:00:02+00:00","workspace":"w"}
{not json
{"data":{"result":{"error":"boom","success":false},"tool_call_id":"t1","tool_name":"bash"},"event":"tool:post","timestamp":"2026-01-01T00:00:03+00:00","workspace":"w"}
"#;
        fs::write(ci.join("events.jsonl"), body).unwrap();

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);

        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[0].content.as_ref().and_then(|c| c.as_str()), Some("hello"));
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        assert_eq!(msgs[2].role.as_deref(), Some("tool"));
        assert_eq!(msgs[2].name.as_deref(), Some("bash"));
        // success:false must survive so detect_errors fires on the tool line.
        let tool_content = msgs[2].content.as_ref().unwrap();
        assert_eq!(tool_content.get("success").and_then(|v| v.as_bool()), Some(false));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_events_returns_session_events() {
        let root = test_dir("load-events");
        let ci = make_session(&root, "sess-ev", Some(VALID_META));
        let body = r#"{"data":{},"event":"session:resume","timestamp":"2026-01-01T09:00:00+00:00","workspace":"w"}
{"data":{},"event":"context:compaction","timestamp":"2026-01-01T09:01:00+00:00","workspace":"w"}
{"data":{"tool_input":{"command":"ls"},"tool_name":"bash"},"event":"tool:pre","timestamp":"2026-01-01T09:02:00+00:00","workspace":"w"}
"#;
        fs::write(ci.join("events.jsonl"), body).unwrap();

        let reader = ContextIntelligenceReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);

        let events = reader
            .load_events(&handles[0])
            .unwrap()
            .expect("context-intelligence always has an event stream");
        assert_eq!(events.len(), 3);
        use crate::reader::SessionEventKind;
        assert_eq!(events[0].kind, SessionEventKind::Resume);
        assert_eq!(events[1].kind, SessionEventKind::Compaction);
        assert_eq!(events[2].kind, SessionEventKind::ToolCall);
        assert_eq!(events[2].tool_name.as_deref(), Some("bash"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn check_metadata_reports_reasons() {
        let root = test_dir("check-meta");
        let ci = make_session(&root, "sess-check", None);

        assert!(check_session_metadata(&ci).unwrap_err().contains("unreadable"));

        fs::write(ci.join("metadata.json"), "{oops").unwrap();
        assert!(check_session_metadata(&ci).unwrap_err().contains("malformed"));

        fs::write(ci.join("metadata.json"), r#"{"format":"context-intelligence"}"#).unwrap();
        assert!(check_session_metadata(&ci).unwrap_err().contains("no version"));

        fs::write(ci.join("metadata.json"), VALID_META).unwrap();
        let meta = check_session_metadata(&ci).unwrap();
        assert_eq!(meta.version.as_deref(), Some("1.0.0"));
        let _ = fs::remove_dir_all(&root);
    }
}
