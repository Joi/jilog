//! NanoclawReader — scans a NanoClaw cell's agent session transcripts.
//!
//! NanoClaw (the jibot cell runtime) runs one Claude Code SDK agent per
//! agent group. Each agent's sessions live under the cell's data dir:
//!
//!   `<data>/v2-sessions/<agent-id>/.claude-shared/projects/<proj>/<uuid>.jsonl`
//!
//! The transcripts are standard Claude Code session format (wrapped
//! `type`+`message` lines) plus NanoClaw-specific `queue-operation` entries
//! (enqueue/dequeue/remove), which are skipped. The cell's routing database
//! (`<data>/v2.db`) maps each `<agent-id>` to a persona (`agent_groups.name`,
//! e.g. "jibot", "bifbot") and the messaging group(s) it serves
//! (`messaging_groups.name` via `messaging_group_agents`), so every handle —
//! and therefore every signal — carries which-bot/which-channel.
//!
//! Trust tiers: an explicit `include`/`exclude` list is matched against each
//! agent's id, persona, AND folder slug (BIF-adjacent groups can run under
//! the `jibot` persona, so persona-level matching alone is not enough).
//! `exclude` always wins; a non-empty `include` admits only matching agents.
//! The filters are enforced against RESOLVED metadata only: whenever any
//! include/exclude list is configured, an agent with no v2.db row — or any
//! agent when the db is absent/unreadable (e.g. a torn mirror copy) — is
//! skipped entirely, because an exclude like `["bifbot"]` cannot be checked
//! against a directory name like `ag-1781087414868-eqq735`. Filtered cells
//! therefore fail closed. Without filters, unmapped agents fall back to
//! their directory name as persona.
//!
//! User messages arrive wrapped in NanoClaw envelope XML
//! (`<context .../>` + one or more `<message ...>text</message>`); `load`
//! extracts the inner text so the correction detectors see real message
//! lengths. Tool results (Claude-format `tool_result` blocks on user-role
//! lines) are re-emitted as `role: "tool"` messages with the
//! `{"success": bool, ...}` shape `detect_errors` expects, mirroring the pi
//! reader. `load_events` yields UserMessage/LlmResponse/ToolCall (and
//! Compaction for compact-summary lines) so the health detectors — the
//! highest-value signals on unattended cells — get real timestamps.
//! `load_stats` sums the Claude usage objects (tokens only; cell transcripts
//! carry no cost field).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use std::sync::OnceLock;

use crate::error::JilogReviewError;
use crate::reader::{
    Message, Reader, SessionEvent, SessionEventKind, SessionStats, TranscriptHandle,
};
use crate::util::parse_iso8601;

/// Reader for NanoClaw cell agent sessions.
pub struct NanoclawReader {
    /// Cell data dir (or a read-only mirror of it) containing `v2-sessions/`.
    pub data_dir: PathBuf,
    /// Routing database path; defaults to `<data_dir>/v2.db`.
    pub db_path: PathBuf,
    /// Allowlist: when non-empty, only agents whose id, persona, or folder
    /// matches an entry are read.
    pub include: Vec<String>,
    /// Denylist: agents whose id, persona, or folder matches are never read.
    /// Wins over `include`.
    pub exclude: Vec<String>,
}

/// Per-agent routing info from v2.db.
#[derive(Debug, Clone)]
struct AgentInfo {
    /// `agent_groups.name` — the persona ("jibot", "bifbot", "canary", …).
    persona: String,
    /// `agent_groups.folder` — human-readable slug ("vibez", "bifbot", …).
    folder: String,
    /// Names of the messaging group(s) this agent serves, joined with ", "
    /// when there are several. None when the agent has no registered group.
    channel: Option<String>,
}

impl NanoclawReader {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        let db_path = data_dir.join("v2.db");
        Self { data_dir, db_path, include: Vec::new(), exclude: Vec::new() }
    }

    pub fn with_db_path(mut self, db_path: impl Into<PathBuf>) -> Self {
        self.db_path = db_path.into();
        self
    }

    pub fn with_allowlist(mut self, include: Vec<String>, exclude: Vec<String>) -> Self {
        self.include = include;
        self.exclude = exclude;
        self
    }

    /// Load the agent-id → persona/folder/channel map from v2.db.
    ///
    /// Any failure (missing file, torn mirror copy, schema drift) degrades to
    /// an empty map with a warning: sessions then carry their agent dir name
    /// as persona, and a non-empty `include` list admits nothing it doesn't
    /// name explicitly.
    fn load_agent_map(&self) -> HashMap<String, AgentInfo> {
        match self.query_agent_map() {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(
                    "nanoclaw: cannot read agent map from {}: {} — personas fall back to agent dir names",
                    self.db_path.display(),
                    e
                );
                HashMap::new()
            }
        }
    }

    fn query_agent_map(&self) -> Result<HashMap<String, AgentInfo>, rusqlite::Error> {
        let conn = rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut stmt = conn.prepare(
            "SELECT ag.id, ag.name, ag.folder, mg.name
             FROM agent_groups ag
             LEFT JOIN messaging_group_agents mga ON mga.agent_group_id = ag.id
             LEFT JOIN messaging_groups mg ON mg.id = mga.messaging_group_id
             ORDER BY ag.id, mga.priority DESC, mga.created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;

        // Aggregate multi-group agents into one joined channel string.
        let mut channels: BTreeMap<String, (String, String, Vec<String>)> = BTreeMap::new();
        for row in rows {
            let (id, persona, folder, group) = row?;
            let entry = channels.entry(id).or_insert((persona, folder, Vec::new()));
            if let Some(g) = group {
                entry.2.push(g);
            }
        }
        Ok(channels
            .into_iter()
            .map(|(id, (persona, folder, groups))| {
                let channel = if groups.is_empty() { None } else { Some(groups.join(", ")) };
                (id, AgentInfo { persona, folder, channel })
            })
            .collect())
    }

    /// True if this agent may be read under the include/exclude lists.
    /// Matched against agent id, persona, and folder; exclude wins.
    fn agent_allowed(&self, agent_id: &str, persona: &str, folder: &str) -> bool {
        let matches = |needle: &String| needle == agent_id || needle == persona || needle == folder;
        if self.exclude.iter().any(matches) {
            return false;
        }
        if !self.include.is_empty() && !self.include.iter().any(matches) {
            return false;
        }
        true
    }
}

impl Reader for NanoclawReader {
    fn name(&self) -> &str {
        "nanoclaw"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();
        let sessions_root = self.data_dir.join("v2-sessions");
        if !sessions_root.exists() {
            return Ok(handles);
        }

        let agent_map = self.load_agent_map();
        let filters_active = !self.include.is_empty() || !self.exclude.is_empty();

        let agent_dirs = std::fs::read_dir(&sessions_root)?;
        for agent_dir in agent_dirs.flatten() {
            if !agent_dir.path().is_dir() {
                continue;
            }
            let agent_id = agent_dir.file_name().to_string_lossy().to_string();
            let info = agent_map.get(&agent_id);

            // Trust filters can only be enforced against resolved metadata:
            // exclude = ["bifbot"] matches nothing about a directory named
            // ag-1781087414868-eqq735, so an unmapped agent under a filtered
            // config MUST fail closed, not fall back to dir names.
            if info.is_none() && filters_active {
                tracing::warn!(
                    "nanoclaw: agent '{}' has no v2.db mapping and a trust filter is configured — skipping (fail closed)",
                    agent_id
                );
                continue;
            }

            let persona =
                info.map(|i| i.persona.clone()).unwrap_or_else(|| agent_id.clone());
            let folder = info.map(|i| i.folder.clone()).unwrap_or_else(|| agent_id.clone());
            let channel = info.and_then(|i| i.channel.clone());

            if !self.agent_allowed(&agent_id, &persona, &folder) {
                continue;
            }

            // Escape the interpolated dir: agent names could contain glob
            // metacharacters, which would silently match nothing.
            let pattern = format!(
                "{}/.claude-shared/projects/**/*.jsonl",
                glob::Pattern::escape(&agent_dir.path().display().to_string())
            );
            let entries = match glob::glob(&pattern) {
                Ok(e) => e,
                Err(e) => {
                    return Err(JilogReviewError::Reader(format!(
                        "nanoclaw: glob error: {}",
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
                        Utc.timestamp_opt(secs as i64, 0).single().unwrap_or_else(Utc::now)
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
                    persona: Some(persona.clone()),
                    channel: channel.clone(),
                });
            }
        }

        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        let content = std::fs::read_to_string(&handle.path)?;
        let mut out = Vec::new();
        // tool_use id → tool name, from assistant content blocks; results
        // always follow their call within the same file.
        let mut tool_names: HashMap<String, String> = HashMap::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // queue-operation, ai-title, mode, last-prompt, attachment, …
            // have no wrapped chat message and are skipped here.
            let line_type = value.get("type").and_then(|v| v.as_str());
            let msg = match value.get("message") {
                Some(m) => m,
                None => continue,
            };
            match (line_type, msg.get("role").and_then(|v| v.as_str())) {
                (Some("user"), Some("user")) => {
                    // Compact summaries recap the pre-compaction conversation
                    // (including already-detected corrections/workarounds/
                    // errors); letting them through double-signals long
                    // sessions. isMeta lines are runtime-injected, not user
                    // activity.
                    if is_flagged(&value, "isCompactSummary") || is_flagged(&value, "isMeta") {
                        continue;
                    }
                    let content = msg.get("content");
                    match content {
                        Some(serde_json::Value::String(s)) => {
                            let text = unwrap_envelope(s);
                            if text.is_empty() {
                                continue;
                            }
                            out.push(Message {
                                role: Some("user".to_string()),
                                content: Some(serde_json::Value::String(text)),
                                name: None,
                            });
                        }
                        Some(serde_json::Value::Array(blocks)) => {
                            // Tool echoes → role "tool" messages in the
                            // success/error shape detect_errors expects.
                            let mut texts: Vec<String> = Vec::new();
                            for block in blocks {
                                match block.get("type").and_then(|v| v.as_str()) {
                                    Some("tool_result") => {
                                        let is_error = block
                                            .get("is_error")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                        let text = block_text(block.get("content"));
                                        let tool_name = block
                                            .get("tool_use_id")
                                            .and_then(|v| v.as_str())
                                            .and_then(|id| tool_names.get(id))
                                            .cloned();
                                        let content = if is_error {
                                            serde_json::json!({"success": false, "error": text})
                                        } else {
                                            serde_json::json!({"success": true, "output": text})
                                        };
                                        out.push(Message {
                                            role: Some("tool".to_string()),
                                            content: Some(content),
                                            name: tool_name,
                                        });
                                    }
                                    Some("text") => {
                                        if let Some(t) =
                                            block.get("text").and_then(|v| v.as_str())
                                        {
                                            texts.push(unwrap_envelope(t));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            let text = texts.join("\n");
                            if !text.trim().is_empty() {
                                out.push(Message {
                                    role: Some("user".to_string()),
                                    content: Some(serde_json::Value::String(text)),
                                    name: None,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                (Some("assistant"), Some("assistant")) => {
                    if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                        for block in blocks {
                            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                if let (Some(id), Some(name)) = (
                                    block.get("id").and_then(|v| v.as_str()),
                                    block.get("name").and_then(|v| v.as_str()),
                                ) {
                                    tool_names.insert(id.to_string(), name.to_string());
                                }
                            }
                        }
                    }
                    // One API response can be written as several assistant
                    // lines (one per content block, same message.id) — each
                    // line carries only its own blocks, so all are kept for
                    // message content; only usage/events need id-dedup.
                    out.push(Message {
                        role: Some("assistant".to_string()),
                        content: msg.get("content").cloned(),
                        name: None,
                    });
                }
                _ => {}
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
        let mut seen_responses: std::collections::HashSet<String> = std::collections::HashSet::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // queue-operation lines carry timestamps but duplicate the user
            // line that follows on delivery — skip to avoid double counting.
            let line_type = value.get("type").and_then(|v| v.as_str());
            if line_type != Some("user") && line_type != Some("assistant") {
                continue;
            }
            let timestamp = match value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(parse_iso8601)
            {
                Some(t) => t,
                None => continue,
            };
            let msg = match value.get("message") {
                Some(m) => m,
                None => continue,
            };
            match msg.get("role").and_then(|v| v.as_str()) {
                Some("user") => {
                    // Compact-summary continuations are compactions, not
                    // user activity.
                    if is_flagged(&value, "isCompactSummary") {
                        out.push(SessionEvent {
                            kind: SessionEventKind::Compaction,
                            timestamp,
                            tool_name: None,
                            detail: None,
                        });
                        continue;
                    }
                    // Runtime-injected meta lines are not user activity —
                    // counting them would reset the iteration-runaway window
                    // (mirrors the load() skip).
                    if is_flagged(&value, "isMeta") {
                        continue;
                    }
                    // Tool echoes (tool_result blocks) are runtime-injected,
                    // not user activity; counting them would mask iteration
                    // runaway (every tool call would "reset" the window).
                    let is_tool_echo = msg
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|blocks| {
                            blocks.iter().any(|b| {
                                b.get("type").and_then(|v| v.as_str()) == Some("tool_result")
                            })
                        })
                        .unwrap_or(false);
                    if !is_tool_echo {
                        out.push(SessionEvent {
                            kind: SessionEventKind::UserMessage,
                            timestamp,
                            tool_name: None,
                            detail: None,
                        });
                    }
                }
                Some("assistant") => {
                    // One API response, several JSONL lines (same
                    // message.id, one per content block): one LlmResponse.
                    // ToolCall blocks are per-line and never duplicated, so
                    // they are emitted unconditionally.
                    let is_new = match response_key(&value) {
                        Some(key) => seen_responses.insert(key),
                        None => true,
                    };
                    if is_new {
                        out.push(SessionEvent {
                            kind: SessionEventKind::LlmResponse,
                            timestamp,
                            tool_name: None,
                            detail: None,
                        });
                    }
                    if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                        for block in blocks {
                            if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                                continue;
                            }
                            let tool_name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            // serde_json's default Map is a BTreeMap, so
                            // to_string() is key-sorted canonical — identical
                            // arguments compare equal as strings.
                            let detail = block.get("input").map(|v| v.to_string());
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
        let mut seen_responses: std::collections::HashSet<String> = std::collections::HashSet::new();

        for line in content.lines() {
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            let usage = match value.get("message").and_then(|m| m.get("usage")) {
                Some(u) if u.is_object() => u,
                _ => continue,
            };
            // A multi-block response is written as several assistant lines,
            // each repeating the SAME message.id and usage object; summing
            // per line would multiply the (large) cache-read numbers.
            if let Some(key) = response_key(&value) {
                if !seen_responses.insert(key) {
                    continue;
                }
            }
            saw_usage = true;
            // All input-side tokens: Claude reports cache reads/writes
            // separately from uncached input.
            for key in ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"] {
                input_tokens += usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            }
            output_tokens += usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        }

        if !saw_usage {
            return Ok(None);
        }
        // Cell transcripts carry token usage but no cost field, so sessions
        // contribute tokens to the Spend section without a dollar total.
        Ok(Some(SessionStats {
            cost_usd: None,
            input_tokens,
            output_tokens,
            role: None,
            model_costs: std::collections::BTreeMap::new(),
        }))
    }
}

/// True if a top-level boolean flag (e.g. `isCompactSummary`, `isMeta`) is
/// set on the line.
fn is_flagged(value: &serde_json::Value, key: &str) -> bool {
    value.get(key).and_then(|v| v.as_bool()) == Some(true)
}

/// Identity of the API response an assistant line belongs to, for deduping
/// split multi-block responses: `message.id`, else `requestId`, else the
/// line `uuid`. None when the line carries no usable identity (callers then
/// treat it as unique).
fn response_key(value: &serde_json::Value) -> Option<String> {
    value
        .get("message")
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .or_else(|| value.get("requestId").and_then(|v| v.as_str()))
        .or_else(|| value.get("uuid").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

/// Extract the human text from a NanoClaw envelope string.
///
/// Envelopes look like `<context .../>` followed by one or more
/// `<message id=".." from=".." sender=".." time="..">text</message>`
/// elements (several when messages were batched). Inner texts are joined
/// with newlines and XML entities unescaped. Strings without a `<message>`
/// element are returned trimmed as-is (canary pings, plain prompts).
fn unwrap_envelope(s: &str) -> String {
    static MESSAGE_RE: OnceLock<Regex> = OnceLock::new();
    let re = MESSAGE_RE
        .get_or_init(|| Regex::new(r"(?s)<message\b[^>]*>(.*?)</message>").expect("static regex"));
    if !re.is_match(s) {
        // No envelope (canary pings, plain prompts): raw string, trimmed.
        return s.trim().to_string();
    }
    let parts: Vec<String> = re
        .captures_iter(s)
        .map(|c| xml_unescape(c[1].trim()))
        .filter(|t| !t.is_empty())
        .collect();
    parts.join("\n")
}

/// Minimal XML entity unescape for envelope text.
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Text of a tool_result block's `content`, which is either a string or an
/// array of `{type: "text", text}` blocks.
fn block_text(content: Option<&serde_json::Value>) -> String {
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
        let dir = std::env::temp_dir().join("jilog-test-nanoclaw").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a v2.db with the real schema subset the reader queries.
    fn write_v2db(data_dir: &PathBuf) {
        let conn = rusqlite::Connection::open(data_dir.join("v2.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE agent_groups (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, folder TEXT NOT NULL UNIQUE,
                agent_provider TEXT, created_at TEXT NOT NULL
            );
            CREATE TABLE messaging_groups (
                id TEXT PRIMARY KEY, channel_type TEXT NOT NULL, platform_id TEXT NOT NULL,
                instance TEXT NOT NULL, name TEXT, is_group INTEGER DEFAULT 0,
                unknown_sender_policy TEXT NOT NULL DEFAULT 'strict', created_at TEXT NOT NULL,
                denied_at TEXT
            );
            CREATE TABLE messaging_group_agents (
                id TEXT PRIMARY KEY, messaging_group_id TEXT NOT NULL,
                agent_group_id TEXT NOT NULL, session_mode TEXT DEFAULT 'shared',
                priority INTEGER DEFAULT 0, created_at TEXT NOT NULL
            );
            INSERT INTO agent_groups VALUES
                ('ag-1', 'jibot', 'vibez', NULL, '2026-05-06'),
                ('ag-2', 'bifbot', 'bifbot', NULL, '2026-05-06'),
                ('ag-3', 'jibot', 'bif-2027-steering', NULL, '2026-05-06');
            INSERT INTO messaging_groups VALUES
                ('mg-1', 'whatsapp', '1@g.us', 'whatsapp', 'The vibez', 1, 'public', '2026-05-06', NULL),
                ('mg-2', 'whatsapp', '2@g.us', 'whatsapp', 'BIF Event Director Group', 1, 'public', '2026-05-06', NULL),
                ('mg-3', 'whatsapp', '3@g.us', 'whatsapp', 'BIF 2027: Steering Committee', 1, 'public', '2026-05-06', NULL);
            INSERT INTO messaging_group_agents VALUES
                ('mga-1', 'mg-1', 'ag-1', 'shared', 0, '2026-05-06'),
                ('mga-2', 'mg-2', 'ag-2', 'shared', 0, '2026-05-06'),
                ('mga-3', 'mg-3', 'ag-3', 'shared', 0, '2026-05-06');
            "#,
        )
        .unwrap();
    }

    /// A realistic minimal cell transcript: queue-operation, envelope user
    /// turn, assistant turn with text + tool_use, tool_result echo (error),
    /// closing assistant turn, and session-meta lines.
    const SESSION_BODY: &str = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-08T10:11:51.540Z","sessionId":"s-1","content":"<message id=\"2\" from=\"whatsapp-mg-1\">@jibot ping</message>"}
{"type":"user","uuid":"u1","timestamp":"2026-07-08T10:11:51.576Z","message":{"role":"user","content":"<context timezone=\"Asia/Thimphu\" />\n<message id=\"2\" from=\"whatsapp-mg-1\" sender=\"819@s.whatsapp.net\" time=\"Jul 8, 2026, 4:11 PM\">no jibot, don&#39;t answer in that channel</message>"},"sessionId":"s-1"}
{"type":"assistant","uuid":"a1","timestamp":"2026-07-08T10:12:00.000Z","message":{"role":"assistant","model":"claude-opus-4-8","content":[{"type":"text","text":"Understood — I will stay quiet there."},{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"true"}}],"usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":42}},"sessionId":"s-1"}
{"type":"user","uuid":"u2","timestamp":"2026-07-08T10:12:01.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","is_error":true,"content":"command not found"}]},"sessionId":"s-1"}
{"type":"assistant","uuid":"a2","timestamp":"2026-07-08T10:12:05.000Z","message":{"role":"assistant","model":"claude-opus-4-8","content":[{"type":"text","text":"That failed."}],"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":600,"output_tokens":7}},"sessionId":"s-1"}
{"type":"ai-title","aiTitle":"quiet channel request","sessionId":"s-1"}
{"type":"last-prompt","leafUuid":"a2","sessionId":"s-1"}
{"type":"mode","mode":"normal","sessionId":"s-1"}
"#;

    fn write_cell(name: &str) -> PathBuf {
        let data = test_dir(name);
        write_v2db(&data);
        for (agent, session) in [("ag-1", "s-1"), ("ag-2", "s-2"), ("ag-3", "s-3"), ("ag-unknown", "s-4")] {
            let proj = data
                .join("v2-sessions")
                .join(agent)
                .join(".claude-shared/projects/-workspace-agent");
            fs::create_dir_all(&proj).unwrap();
            fs::write(proj.join(format!("{session}.jsonl")), SESSION_BODY).unwrap();
        }
        data
    }

    fn discover_all(reader: &NanoclawReader) -> Vec<TranscriptHandle> {
        reader.discover(Utc::now() - Duration::days(1)).unwrap()
    }

    #[test]
    fn nanoclaw_discovers_and_maps_persona_channel() {
        let data = write_cell("discover");
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        assert_eq!(handles.len(), 4);

        let by_id: HashMap<&str, &TranscriptHandle> =
            handles.iter().map(|h| (h.session_id.as_str(), h)).collect();
        assert_eq!(by_id["s-1"].persona.as_deref(), Some("jibot"));
        assert_eq!(by_id["s-1"].channel.as_deref(), Some("The vibez"));
        assert_eq!(by_id["s-2"].persona.as_deref(), Some("bifbot"));
        assert_eq!(by_id["s-2"].channel.as_deref(), Some("BIF Event Director Group"));
        // Unknown agent dir: persona falls back to the dir name, no channel.
        assert_eq!(by_id["s-4"].persona.as_deref(), Some("ag-unknown"));
        assert_eq!(by_id["s-4"].channel, None);
        assert_eq!(by_id["s-1"].reader_name, "nanoclaw");
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_exclude_matches_persona_and_folder() {
        let data = write_cell("exclude");
        // Trust tier: bifbot persona AND the BIF-adjacent jibot folder out.
        let reader = NanoclawReader::new(&data).with_allowlist(
            Vec::new(),
            vec!["bifbot".to_string(), "bif-2027-steering".to_string()],
        );
        let handles = discover_all(&reader);
        let ids: Vec<&str> = handles.iter().map(|h| h.session_id.as_str()).collect();
        // s-4's agent has no v2.db row: with a filter configured it is
        // skipped too (fail closed) — its dir name proves nothing about
        // whether it is BIF-adjacent.
        assert_eq!(ids, vec!["s-1"], "bifbot (persona), steering (folder), unmapped (fail closed) all excluded");
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_include_is_explicit_allowlist() {
        let data = write_cell("include");
        let reader = NanoclawReader::new(&data)
            .with_allowlist(vec!["jibot".to_string()], Vec::new());
        let handles = discover_all(&reader);
        let ids: Vec<&str> = handles.iter().map(|h| h.session_id.as_str()).collect();
        // Both jibot agents (vibez + steering) match by persona; bifbot and
        // the unknown dir don't.
        assert_eq!(ids, vec!["s-1", "s-3"]);

        // exclude wins over include.
        let reader = NanoclawReader::new(&data).with_allowlist(
            vec!["jibot".to_string()],
            vec!["bif-2027-steering".to_string()],
        );
        let ids: Vec<String> = discover_all(&reader)
            .iter()
            .map(|h| h.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["s-1"]);
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_missing_db_defaults_personas_to_dir_names() {
        let data = write_cell("no-db");
        fs::remove_file(data.join("v2.db")).unwrap();
        // No filters: sessions still flow, personas fall back to dir names.
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        assert_eq!(handles.len(), 4, "missing db must not drop unfiltered sessions");
        assert!(handles.iter().all(|h| h.channel.is_none()));
        // An explicit include list admits nothing it can't resolve.
        let reader = NanoclawReader::new(&data)
            .with_allowlist(vec!["jibot".to_string()], Vec::new());
        assert!(discover_all(&reader).is_empty());
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_exclude_only_fails_closed_without_db() {
        // THE trust-boundary case: exclude = ["bifbot"] with an unreadable
        // db. Dir-name fallback would never match "bifbot", so the filter
        // must refuse to read anything rather than fail open and leak the
        // excluded agent's sessions.
        let data = write_cell("no-db-exclude");
        fs::remove_file(data.join("v2.db")).unwrap();
        let reader = NanoclawReader::new(&data)
            .with_allowlist(Vec::new(), vec!["bifbot".to_string()]);
        assert!(
            discover_all(&reader).is_empty(),
            "exclude-only config must fail closed when the db is unreadable"
        );
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_load_unwraps_envelope_and_maps_tool_results() {
        let data = write_cell("load");
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        let h = handles.iter().find(|h| h.session_id == "s-1").unwrap();
        let msgs = reader.load(h).unwrap();

        assert_eq!(msgs.len(), 4, "user, assistant, tool, assistant");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(
            msgs[0].content.as_ref().and_then(|c| c.as_str()),
            Some("no jibot, don't answer in that channel"),
            "envelope XML stripped, entities unescaped"
        );
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        // Failing tool_result → role tool, named via the tool_use id.
        assert_eq!(msgs[2].role.as_deref(), Some("tool"));
        assert_eq!(msgs[2].name.as_deref(), Some("Bash"));
        let tool_content = msgs[2].content.as_ref().unwrap();
        assert_eq!(tool_content.get("success").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            tool_content.get("error").and_then(|v| v.as_str()),
            Some("command not found")
        );
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_events_skip_queue_ops_and_tool_echoes() {
        let data = write_cell("events");
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        let h = handles.iter().find(|h| h.session_id == "s-1").unwrap();
        let events = reader.load_events(h).unwrap().expect("cell sessions have events");

        let kinds: Vec<SessionEventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SessionEventKind::UserMessage, // envelope user turn (enqueue skipped)
                SessionEventKind::LlmResponse,
                SessionEventKind::ToolCall,
                // tool_result echo intentionally NOT a UserMessage
                SessionEventKind::LlmResponse,
            ]
        );
        let call = &events[2];
        assert_eq!(call.tool_name.as_deref(), Some("Bash"));
        assert_eq!(call.detail.as_deref(), Some(r#"{"command":"true"}"#));
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_stats_sum_tokens_without_cost() {
        let data = write_cell("stats");
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        let h = handles.iter().find(|h| h.session_id == "s-1").unwrap();
        let stats = reader.load_stats(h).unwrap().expect("usage present");

        // input-side: (100+200+300) + (10+0+600); output: 42 + 7.
        assert_eq!(stats.input_tokens, 1210);
        assert_eq!(stats.output_tokens, 49);
        assert_eq!(stats.cost_usd, None, "cell transcripts carry no cost field");
        assert!(stats.model_costs.is_empty());
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn unwrap_envelope_forms() {
        // Multiple batched messages join with newlines.
        let batched = r#"<context tz="x" />
<message id="1" from="mg">first line</message>
<message id="2" from="mg">second &amp; third</message>"#;
        assert_eq!(unwrap_envelope(batched), "first line\nsecond & third");
        // No envelope → trimmed as-is.
        assert_eq!(unwrap_envelope("  plain prompt  "), "plain prompt");
        // Empty message body → falls back to nothing.
        assert_eq!(unwrap_envelope(r#"<message id="1"></message>"#), "");
    }

    #[test]
    fn nanoclaw_split_multiblock_response_counts_usage_once() {
        // One API response written as TWO assistant lines (same message.id,
        // one content block each, identical usage) — the real Claude Code
        // split shape. Usage must count once; LlmResponse must emit once;
        // both lines' content is kept.
        let data = test_dir("split");
        write_v2db(&data);
        let proj = data.join("v2-sessions/ag-1/.claude-shared/projects/-workspace-agent");
        fs::create_dir_all(&proj).unwrap();
        let body = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-07-08T10:00:00.000Z","message":{"id":"msg_01","role":"assistant","content":[{"type":"text","text":"part one"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":5000,"output_tokens":30}},"sessionId":"s-s"}
{"type":"assistant","uuid":"a2","timestamp":"2026-07-08T10:00:00.500Z","message":{"id":"msg_01","role":"assistant","content":[{"type":"tool_use","id":"toolu_02","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":5000,"output_tokens":30}},"sessionId":"s-s"}
{"type":"assistant","uuid":"a3","timestamp":"2026-07-08T10:01:00.000Z","message":{"id":"msg_02","role":"assistant","content":[{"type":"text","text":"second response"}],"usage":{"input_tokens":1,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":4}},"sessionId":"s-s"}
"#;
        fs::write(proj.join("s-s.jsonl"), body).unwrap();
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        let h = handles.iter().find(|h| h.session_id == "s-s").unwrap();

        let stats = reader.load_stats(h).unwrap().unwrap();
        // msg_01 counted once: (10+20+5000) + msg_02 (1+2+3); out 30 + 4.
        assert_eq!(stats.input_tokens, 5036);
        assert_eq!(stats.output_tokens, 34);

        let events = reader.load_events(h).unwrap().unwrap();
        let responses = events
            .iter()
            .filter(|e| e.kind == SessionEventKind::LlmResponse)
            .count();
        assert_eq!(responses, 2, "one LlmResponse per API response, not per line");
        let tool_calls = events
            .iter()
            .filter(|e| e.kind == SessionEventKind::ToolCall)
            .count();
        assert_eq!(tool_calls, 1, "tool_use block still emitted");

        let msgs = reader.load(h).unwrap();
        assert_eq!(msgs.len(), 3, "split lines keep their distinct content blocks");
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_load_skips_compact_summary_and_meta_lines() {
        let data = test_dir("load-compact");
        write_v2db(&data);
        let proj = data.join("v2-sessions/ag-1/.claude-shared/projects/-workspace-agent");
        fs::create_dir_all(&proj).unwrap();
        // The compact summary quotes correction/workaround language from the
        // pre-compaction conversation; it must not reach the detectors.
        let body = r#"{"type":"user","isCompactSummary":true,"timestamp":"2026-07-08T10:00:00.000Z","message":{"role":"user","content":"Summary: user said don't post there; assistant used a workaround for now."},"sessionId":"s-m"}
{"type":"user","isMeta":true,"timestamp":"2026-07-08T10:00:01.000Z","message":{"role":"user","content":"runtime-injected meta line"},"sessionId":"s-m"}
{"type":"user","uuid":"u1","timestamp":"2026-07-08T10:00:02.000Z","message":{"role":"user","content":"<message id=\"1\" from=\"mg\">a real user message</message>"},"sessionId":"s-m"}
"#;
        fs::write(proj.join("s-m.jsonl"), body).unwrap();
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 1, "only the real user message survives");
        assert_eq!(
            msgs[0].content.as_ref().and_then(|c| c.as_str()),
            Some("a real user message")
        );
        // Events view must agree: compaction + one real UserMessage, and no
        // UserMessage for the isMeta line.
        let events = reader.load_events(&handles[0]).unwrap().unwrap();
        let kinds: Vec<SessionEventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![SessionEventKind::Compaction, SessionEventKind::UserMessage]);
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn nanoclaw_compact_summary_becomes_compaction_event() {
        let data = test_dir("compact");
        write_v2db(&data);
        let proj = data
            .join("v2-sessions/ag-1/.claude-shared/projects/-workspace-agent");
        fs::create_dir_all(&proj).unwrap();
        let body = r#"{"type":"user","isCompactSummary":true,"timestamp":"2026-07-08T10:00:00.000Z","message":{"role":"user","content":"This session is being continued from a previous conversation..."},"sessionId":"s-c"}
{"type":"assistant","timestamp":"2026-07-08T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Continuing."}]},"sessionId":"s-c"}
"#;
        fs::write(proj.join("s-c.jsonl"), body).unwrap();
        let reader = NanoclawReader::new(&data);
        let handles = discover_all(&reader);
        let events = reader.load_events(&handles[0]).unwrap().unwrap();
        let kinds: Vec<SessionEventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![SessionEventKind::Compaction, SessionEventKind::LlmResponse]);
        let _ = fs::remove_dir_all(&data);
    }
}
