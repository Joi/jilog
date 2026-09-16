//! AgentsviewReader — sessions, messages and usage from an agentsview
//! daemon (kenn-io/agentsview) over its REST API (jilog#heyg).
//!
//! The archive covers every agent agentsview syncs (Claude Code pool seats
//! and profiles, Codex pool, cowork, cursor, copilot, hermes, pi, …) and
//! every machine it collects from. This reader turns each archived session
//! into a [`TranscriptHandle`] whose id is the archive id (`<agent>:<id>`;
//! Claude sessions are the bare transcript uuid), loads its user/assistant
//! rows as Schema-B messages, reports per-session usage as
//! [`SessionStats`], and tags signals with the agent and the machine label.
//!
//! Dedupe: [`Reader::dedupe_key`] strips the agent prefix, so a session a
//! raw reader already scanned (same uuid) is skipped by `run_review`. List
//! this reader AFTER the raw readers in `jilog.toml`.
//!
//! No error signals: the archive's message rows carry no tool name (tool
//! identity lives under `/sessions/{id}/tool-calls`), so only `user` and
//! `assistant` rows are loaded and `detect_errors` is never fed a row it
//! would attribute to tool "unknown".
//!
//! Transport: plain HTTP (`ureq`, no TLS) to an IP-literal origin — the
//! daemon is loopback or tailnet-HTTP, and a hostname would put DNS
//! resolution outside the request timeout. `Authorization: Bearer <token>`;
//! the token is read from `token_file` (a TOML with `auth_token`, or a bare
//! token) at request time and never logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, SessionStats, TranscriptHandle};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_TOKEN_FILE: &str = "~/.agentsview/config.toml";
pub const DEFAULT_SINCE_DAYS: u32 = 7;
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Sessions (and messages) per page — the daemon's maximum.
const PAGE_LIMIT: usize = 500;
/// Pagination ceiling per call (500 × 40 = 20k sessions or messages).
const MAX_PAGES: usize = 40;

/// One archived session as listed by `/api/v1/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSession {
    pub id: String,
    pub agent: String,
    /// Opaque machine key; `/api/v1/machines` maps it to a label.
    pub machine: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

pub struct AgentsviewReader {
    base_url: String,
    token_file: PathBuf,
    since_days: u32,
    timeout: Duration,
    /// id → (agent, machine key), filled by `discover`.
    sessions: Mutex<HashMap<String, (String, String)>>,
    /// machine key → label, filled by `discover`.
    labels: Mutex<HashMap<String, String>>,
}

/// The daemon ORIGIN, normalized: `http://<ip>[:port]` with an IP-literal
/// host (v4, or bracketed v6) and nothing after the authority (a single
/// trailing `/` is tolerated and trimmed). Refused: any other scheme (no
/// TLS support), a hostname (ureq cannot bound DNS resolution by the
/// timeout — the nightly could hang on a dead resolver), userinfo, a path,
/// a query, a fragment, or a bad port. Called by `JilogConfig::from_toml_str`
/// so a bad url fails at config load, and again by `AgentsviewReader::new`.
pub fn validate_url(url: &str) -> Result<String, JilogReviewError> {
    let bad = |why: &str| {
        JilogReviewError::Reader(format!(
            "agentsview: url must be http://<ip>[:port] ({}; no TLS, no DNS): {}",
            why, url
        ))
    };
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(|| bad("scheme must be http://"))?
        .trim_end_matches('/');
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return Err(bad("origin only — no path, query, fragment or userinfo"));
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (h, after) = rest
                .split_once(']')
                .ok_or_else(|| bad("unterminated IPv6 literal"))?;
            (h.to_string(), after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        },
    };
    host.parse::<std::net::IpAddr>()
        .map_err(|_| bad("host must be an IP literal"))?;
    if let Some(p) = port {
        p.parse::<u16>().map_err(|_| bad("port must be 0-65535"))?;
    }
    Ok(format!("http://{}", authority))
}

/// Read the bearer token. A body that parses as a TOML table is a config
/// file and MUST carry a string `auth_token` — a table without it is an
/// error, never a fallback to the body (another setting's value must not
/// be sent as a bearer). A body that is not TOML is a bare token, trimmed.
/// Errors name the path, never the contents.
pub fn read_token(path: &Path) -> Result<String, JilogReviewError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        JilogReviewError::Reader(format!("agentsview: token file {}: {}", path.display(), e))
    })?;
    let token = match raw.parse::<toml::Table>() {
        Ok(table) if !table.is_empty() => match table.get("auth_token").and_then(|v| v.as_str()) {
            Some(t) => t.trim().to_string(),
            None => {
                return Err(JilogReviewError::Reader(format!(
                    "agentsview: token file {} is a TOML config with no auth_token",
                    path.display()
                )))
            }
        },
        _ => raw.trim().to_string(),
    };
    if token.is_empty() {
        return Err(JilogReviewError::Reader(format!(
            "agentsview: token file {} is empty (expected auth_token = \"…\" or a bare token)",
            path.display()
        )));
    }
    Ok(token)
}

/// `<agent>:<id>` → `<id>`; ids without a prefix (Claude) are returned as is.
pub fn strip_agent_prefix(id: &str) -> &str {
    match id.split_once(':') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => id,
    }
}

/// agentsview prices in integer microdollars; six decimal places, exact.
pub fn microdollars_to_usd(m: i64) -> Decimal {
    Decimal::new(m, 6)
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn time_field(v: &serde_json::Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// One `/api/v1/sessions` page → (sessions, next cursor).
pub fn parse_sessions_page(v: &serde_json::Value) -> (Vec<ArchiveSession>, Option<String>) {
    let sessions: Vec<ArchiveSession> = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(ArchiveSession {
                        id: str_field(row, "id")?,
                        agent: str_field(row, "agent").unwrap_or_default(),
                        machine: str_field(row, "machine").unwrap_or_default(),
                        started_at: time_field(row, "started_at"),
                        ended_at: time_field(row, "ended_at"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (sessions, str_field(v, "next_cursor").filter(|c| !c.is_empty()))
}

/// One `/api/v1/sessions/{id}/messages` page → (messages, `count`,
/// `last_ordinal`). Only `user` and `assistant` rows become messages: the
/// archive's rows carry no tool name, so a `tool` row would reach
/// `detect_errors` as tool "unknown" and bypass the expected-noise rules.
pub fn parse_messages_page(v: &serde_json::Value) -> (Vec<Message>, usize, Option<i64>) {
    let messages: Vec<Message> = v
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let role = str_field(row, "role")?;
                    if role != "user" && role != "assistant" {
                        return None;
                    }
                    let content = row
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(Message {
                        role: Some(role),
                        content: Some(serde_json::Value::String(content)),
                        name: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let count = v
        .get("count")
        .and_then(|c| c.as_u64())
        .unwrap_or(messages.len() as u64) as usize;
    let last = v.get("last_ordinal").and_then(|c| c.as_i64());
    (messages, count, last)
}

/// `/api/v1/sessions/{id}/usage?breakdown=true` → stats, or `None` when the
/// archive has neither token data nor a cost for the session.
pub fn parse_usage(v: &serde_json::Value) -> Option<SessionStats> {
    let has_tokens = v.get("has_token_data").and_then(|b| b.as_bool()).unwrap_or(false);
    let has_cost = v.get("has_cost").and_then(|b| b.as_bool()).unwrap_or(false);
    if !has_tokens && !has_cost {
        return None;
    }
    let micro = |x: &serde_json::Value| {
        x.get("cost")
            .and_then(|c| c.get("microdollars"))
            .and_then(|m| m.as_i64())
    };
    let mut stats = SessionStats {
        cost_usd: if has_cost {
            micro(v).map(|m| microdollars_to_usd(m).to_string())
        } else {
            None
        },
        input_tokens: 0,
        output_tokens: v.get("total_output_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
        role: None,
        model_costs: Default::default(),
    };
    let mut model_costs: std::collections::BTreeMap<String, Decimal> = Default::default();
    for row in v.get("breakdown").and_then(|b| b.as_array()).into_iter().flatten() {
        stats.input_tokens += row.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
        let priced = row.get("has_cost").and_then(|b| b.as_bool()).unwrap_or(false);
        if let (true, Some(model), Some(m)) = (priced, str_field(row, "model"), micro(row)) {
            *model_costs.entry(model).or_insert(Decimal::ZERO) += microdollars_to_usd(m);
        }
    }
    stats.model_costs = model_costs
        .into_iter()
        .map(|(k, d)| (k, d.to_string()))
        .collect();
    Some(stats)
}

/// `/api/v1/machines` → machine key → label.
pub fn parse_machines(v: &serde_json::Value) -> HashMap<String, String> {
    let labels: HashMap<String, String> = v
        .get("machine_labels")
        .and_then(|m| m.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    labels
}

impl AgentsviewReader {
    pub fn new(
        url: &str,
        token_file: PathBuf,
        since_days: u32,
        timeout: Duration,
    ) -> Result<Self, JilogReviewError> {
        let base_url = validate_url(url)?;
        Ok(Self {
            base_url,
            token_file,
            since_days,
            timeout,
            sessions: Mutex::new(HashMap::new()),
            labels: Mutex::new(HashMap::new()),
        })
    }

    /// GET `<base>/api/v1<path_and_query>` with the bearer token, parsed as
    /// JSON. Errors name the url and the failure class, never the header.
    fn get(&self, path_and_query: &str) -> Result<serde_json::Value, JilogReviewError> {
        let token = read_token(&self.token_file)?;
        let url = format!("{}/api/v1{}", self.base_url, path_and_query);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.timeout)
            .timeout(self.timeout)
            .build();
        let resp = match agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", token))
            .set("Accept", "application/json")
            .call()
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) => {
                return Err(JilogReviewError::Reader(format!(
                    "agentsview: {} → HTTP {}",
                    url, code
                )));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(JilogReviewError::Reader(format!(
                    "agentsview: {} unreachable: {:?}",
                    url,
                    t.kind()
                )));
            }
        };
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut body)
            .map_err(|e| JilogReviewError::Reader(format!("agentsview: {} read: {}", url, e)))?;
        serde_json::from_slice(&body)
            .map_err(|e| JilogReviewError::Reader(format!("agentsview: {} bad JSON: {}", url, e)))
    }

    /// Bounded reachability check (`GET /api/v1/machines`). The CLI runs
    /// it before the `usage daily` spend fetch, which would otherwise
    /// answer from a stale local archive while the daemon is down.
    pub fn probe(&self) -> Result<(), JilogReviewError> {
        self.get("/machines").map(|_| ())
    }
}

impl Reader for AgentsviewReader {
    fn name(&self) -> &str {
        "agentsview"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        // since_days is bounded (1..=3650) at config load, so the day
        // arithmetic cannot overflow.
        let floor = Utc::now() - chrono::Duration::days(self.since_days as i64);
        let since_eff = std::cmp::max(since, floor);
        match self.get("/machines") {
            Ok(v) => *self.labels.lock().unwrap() = parse_machines(&v),
            Err(e) => tracing::warn!("agentsview: machine labels unavailable, using keys: {}", e),
        }
        let mut handles = Vec::new();
        let mut cursor: Option<String> = None;
        let mut index = self.sessions.lock().unwrap();
        for _ in 0..MAX_PAGES {
            let mut query = format!(
                "/sessions?limit={}&active_since={}&include_automated=true&include_one_shot=true&include_children=true",
                PAGE_LIMIT,
                since_eff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            );
            if let Some(c) = &cursor {
                query.push_str("&cursor=");
                query.push_str(c);
            }
            let (page, next) = parse_sessions_page(&self.get(&query)?);
            for s in page {
                // A session without `ended_at` is still active: it was
                // modified "now", whatever its `started_at` says — a
                // long-running session that started before `since` but
                // had activity inside the window must not be dropped.
                let modified = s.ended_at.unwrap_or_else(Utc::now);
                if modified < since {
                    continue;
                }
                index.insert(s.id.clone(), (s.agent.clone(), s.machine.clone()));
                handles.push(TranscriptHandle {
                    session_id: s.id.clone(),
                    path: PathBuf::from(format!("agentsview://{}", s.id)),
                    modified,
                    reader_name: self.name().to_string(),
                    persona: None,
                    channel: None,
                });
            }
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        let mut out = Vec::new();
        let mut from: i64 = 0;
        for _ in 0..MAX_PAGES {
            let v = self.get(&format!(
                "/sessions/{}/messages?from={}&limit={}&direction=asc",
                handle.session_id, from, PAGE_LIMIT
            ))?;
            let (msgs, count, last) = parse_messages_page(&v);
            out.extend(msgs);
            match last {
                Some(l) if count >= PAGE_LIMIT => from = l + 1,
                _ => break,
            }
        }
        Ok(out)
    }

    fn load_stats(
        &self,
        handle: &TranscriptHandle,
    ) -> Result<Option<SessionStats>, JilogReviewError> {
        match self.get(&format!("/sessions/{}/usage?breakdown=true", handle.session_id)) {
            Ok(v) => Ok(parse_usage(&v)),
            // Usage is advisory: a session the archive cannot price is
            // simply a session without stats.
            Err(JilogReviewError::Reader(msg)) if msg.contains("HTTP 404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn agent(&self, handle: &TranscriptHandle) -> Option<String> {
        let agent = self.sessions.lock().unwrap().get(&handle.session_id)?.0.clone();
        (!agent.is_empty()).then_some(agent)
    }

    fn machine(&self, handle: &TranscriptHandle) -> Option<String> {
        let key = self.sessions.lock().unwrap().get(&handle.session_id)?.1.clone();
        if key.is_empty() {
            return None;
        }
        Some(self.labels.lock().unwrap().get(&key).cloned().unwrap_or(key))
    }

    fn dedupe_key(&self, handle: &TranscriptHandle) -> String {
        strip_agent_prefix(&handle.session_id).to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests — fixture JSON (trimmed copies of the live v0.43.0 shapes) and a
// loopback stub server. No external network.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SESSIONS_PAGE_1: &str = r#"{"sessions":[
      {"id":"codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373","project":"i_wa","machine":"4a70f2a82b531088b36678f1b96a47f8","agent":"codex","started_at":"2026-09-15T21:52:03.487Z","ended_at":"2026-09-16T00:00:19.938Z","is_automated":false},
      {"id":"a37ffc87-2799-4a09-830b-a92fde71d768","project":"jibot","machine":"4a70f2a82b531088b36678f1b96a47f8","agent":"claude","started_at":"2026-09-11T21:57:31.589Z","ended_at":"2026-09-11T23:10:00Z","is_automated":true}
    ],"next_cursor":"CURSOR1","total":3}"#;
    const SESSIONS_PAGE_2: &str = r#"{"sessions":[
      {"id":"cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07","project":"x","machine":"deadbeef","agent":"cowork","started_at":"2026-09-15T01:00:00Z","ended_at":null}
    ],"next_cursor":null,"total":3}"#;
    const MESSAGES_PAGE: &str = r#"{"count":4,"first_ordinal":0,"last_ordinal":3,"messages":[
      {"ordinal":0,"role":"user","content":"clean up the worktree","timestamp":"2026-09-11T21:57:31.589Z","model":""},
      {"ordinal":1,"role":"assistant","content":"On it.","timestamp":"2026-09-11T21:57:35.717Z","model":"claude-fable-5-1"},
      {"ordinal":2,"content":"no role here"},
      {"ordinal":3,"role":"tool","content":"{\"success\":false}"}
    ]}"#;
    const USAGE: &str = r#"{"session_id":"a37ffc87-2799-4a09-830b-a92fde71d768","agent":"claude","total_output_tokens":360,"peak_context_tokens":326390,"has_token_data":true,"cost":{"microdollars":717783},"has_cost":true,"cost_usd":0.717783,"models":["claude-fable-5-1"],"unpriced_models":[],"breakdown_count":2,"breakdown":[
      {"ordinal":1,"model":"claude-fable-5-1","input_tokens":2,"output_tokens":174,"cache_creation_input_tokens":29284,"cache_read_input_tokens":34443,"cost":{"microdollars":603011},"has_cost":true},
      {"ordinal":2,"model":"claude-fable-5-1","input_tokens":30,"output_tokens":186,"cache_creation_input_tokens":4476,"cache_read_input_tokens":63727,"cost":{"microdollars":114772},"has_cost":true}
    ]}"#;
    const USAGE_UNPRICED: &str = r#"{"session_id":"x","agent":"cowork","total_output_tokens":10,"has_token_data":true,"cost":{"microdollars":0},"has_cost":false,"models":["mystery"],"unpriced_models":["mystery"],"breakdown_count":1,"breakdown":[{"ordinal":1,"model":"mystery","input_tokens":5,"output_tokens":10,"cost":{"microdollars":0},"has_cost":false}]}"#;
    const USAGE_EMPTY: &str = r#"{"session_id":"x","agent":"codex","total_output_tokens":0,"has_token_data":false,"cost":{"microdollars":0},"has_cost":false,"models":[],"breakdown_count":0,"breakdown":[]}"#;
    const MACHINES: &str = r#"{"machines":["4a70f2a82b531088b36678f1b96a47f8"],"machine_labels":{"4a70f2a82b531088b36678f1b96a47f8":"macazbd"},"machine_aliases":{"local":"4a70f2a82b531088b36678f1b96a47f8"}}"#;

    #[test]
    fn parses_sessions_messages_usage_and_machines() {
        let (page, cursor) = parse_sessions_page(&serde_json::from_str(SESSIONS_PAGE_1).unwrap());
        assert_eq!(cursor.as_deref(), Some("CURSOR1"));
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].agent, "codex");
        assert!(page[0].ended_at.is_some());
        let (page2, cursor2) = parse_sessions_page(&serde_json::from_str(SESSIONS_PAGE_2).unwrap());
        assert_eq!(cursor2, None);
        assert_eq!(page2[0].ended_at, None, "null ended_at is None");

        let (msgs, count, last) = parse_messages_page(&serde_json::from_str(MESSAGES_PAGE).unwrap());
        assert_eq!((count, last), (4, Some(3)));
        assert_eq!(msgs.len(), 2, "role-less and tool rows are dropped: no tool identity, so no error signals");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[1].content.as_ref().and_then(|c| c.as_str()), Some("On it."));

        let stats = parse_usage(&serde_json::from_str(USAGE).unwrap()).expect("priced session");
        assert_eq!(stats.cost_usd.as_deref(), Some("0.717783"));
        assert_eq!(stats.input_tokens, 32);
        assert_eq!(stats.output_tokens, 360);
        assert_eq!(stats.model_costs.get("claude-fable-5-1").map(String::as_str), Some("0.717783"));
        let unpriced = parse_usage(&serde_json::from_str(USAGE_UNPRICED).unwrap()).expect("tokens without cost");
        assert_eq!(unpriced.cost_usd, None);
        assert_eq!(unpriced.input_tokens, 5);
        assert!(unpriced.model_costs.is_empty());
        assert_eq!(parse_usage(&serde_json::from_str(USAGE_EMPTY).unwrap()), None);

        let labels = parse_machines(&serde_json::from_str(MACHINES).unwrap());
        assert_eq!(labels.get("4a70f2a82b531088b36678f1b96a47f8").map(String::as_str), Some("macazbd"));

        assert_eq!(strip_agent_prefix("codex:abc"), "abc");
        assert_eq!(
            strip_agent_prefix("a37ffc87-2799-4a09-830b-a92fde71d768"),
            "a37ffc87-2799-4a09-830b-a92fde71d768"
        );
        assert_eq!(strip_agent_prefix("weird:"), "weird:");
        assert_eq!(microdollars_to_usd(52872492).to_string(), "52.872492");
        assert_eq!(microdollars_to_usd(0).to_string(), "0.000000");
    }

    #[test]
    fn read_token_accepts_toml_and_bare_files_and_rejects_empty_or_tokenless_toml() {
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("config.toml");
        std::fs::write(&toml, "require_auth = true\nauth_token = \"tok-123\"\n[agents]\n").unwrap();
        assert_eq!(read_token(&toml).unwrap(), "tok-123");
        let bare = dir.path().join("token");
        std::fs::write(&bare, "  tok-456\n").unwrap();
        assert_eq!(read_token(&bare).unwrap(), "tok-456");
        // A TOML file WITHOUT auth_token is a config file, never a token:
        // its body must not be sent anywhere.
        let other = dir.path().join("other.toml");
        std::fs::write(&other, "cursor_secret = \"sec-789\"\n").unwrap();
        let err = read_token(&other).unwrap_err().to_string();
        assert!(err.contains("no auth_token"), "{err}");
        assert!(!err.contains("sec-789"), "errors never echo file contents: {err}");
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "\n").unwrap();
        let err = read_token(&empty).unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
        assert!(!err.contains("tok-"), "errors never echo token material");
        assert!(read_token(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn validate_url_accepts_only_http_ip_origins() {
        assert_eq!(validate_url("http://127.0.0.1:8080/").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(validate_url("http://100.64.0.9:8080").unwrap(), "http://100.64.0.9:8080");
        assert_eq!(validate_url("http://[::1]:8080").unwrap(), "http://[::1]:8080");
        assert_eq!(validate_url("http://127.0.0.1").unwrap(), "http://127.0.0.1");
        for bad in [
            "https://127.0.0.1:8080",       // no TLS
            "http://localhost:8080",        // hostname → DNS → unbounded
            "http://collector:8080",
            "http://127.0.0.1:8080/api/v1", // path
            "http://127.0.0.1:8080?x=1",
            "http://127.0.0.1:8080#f",
            "http://user@127.0.0.1:8080",
            "http://127.0.0.1:99999",
            "http://[::1",
            "http://",
            "127.0.0.1:8080",
        ] {
            let err = validate_url(bad).unwrap_err().to_string();
            assert!(err.contains("http://<ip>[:port]"), "{bad}: {err}");
        }
        assert!(AgentsviewReader::new("https://x", PathBuf::from("/nonexistent"), 7, Duration::from_secs(1)).is_err());
        assert!(AgentsviewReader::new("http://127.0.0.1:8080/", PathBuf::from("/x"), 7, Duration::from_secs(1)).is_ok());
    }

    /// Minimal HTTP/1.1 stub: serves canned JSON by path substring (first
    /// matching route wins, so list the more specific route first) and
    /// records the request path + Authorization header of every request.
    fn stub_server(
        routes: Vec<(&'static str, String)>,
    ) -> (String, std::sync::Arc<Mutex<Vec<(String, String)>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let request = line.trim().to_string();
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Authorization: ") {
                        auth = v.trim().to_string();
                    }
                }
                let path = request.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push((path.clone(), auth));
                let body = routes
                    .iter()
                    .find(|(p, _)| path.contains(p))
                    .map(|(_, b)| b.clone());
                let (status, body) = match body {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", "{}".to_string()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
            }
        });
        (base, seen)
    }

    #[test]
    fn reader_pages_sessions_tags_machines_and_sends_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("config.toml");
        std::fs::write(&token_file, "auth_token = \"secret-1\"\n").unwrap();
        let (base, seen) = stub_server(vec![
            ("cursor=CURSOR1", SESSIONS_PAGE_2.to_string()),
            ("/api/v1/sessions?limit=500", SESSIONS_PAGE_1.to_string()),
            ("/api/v1/machines", MACHINES.to_string()),
            ("a37ffc87-2799-4a09-830b-a92fde71d768/messages", MESSAGES_PAGE.to_string()),
            ("a37ffc87-2799-4a09-830b-a92fde71d768/usage", USAGE.to_string()),
            ("cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07/usage", USAGE_EMPTY.to_string()),
        ]);
        let reader = AgentsviewReader::new(&base, token_file, 3650, Duration::from_secs(5)).unwrap();
        let since = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z").unwrap().with_timezone(&Utc);
        let handles = reader.discover(since).unwrap();
        let mut ids: Vec<&str> = handles.iter().map(|h| h.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "a37ffc87-2799-4a09-830b-a92fde71d768",
                "codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373",
                "cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07",
            ],
            "both pages, automated included"
        );
        let claude = handles.iter().find(|h| h.session_id.starts_with("a37f")).unwrap();
        let cowork = handles.iter().find(|h| h.session_id.starts_with("cowork")).unwrap();
        assert_eq!(reader.agent(claude).as_deref(), Some("claude"));
        assert_eq!(reader.agent(cowork).as_deref(), Some("cowork"));
        assert_eq!(reader.machine(claude).as_deref(), Some("macazbd"), "label from /machines");
        assert_eq!(reader.machine(cowork).as_deref(), Some("deadbeef"), "unknown key falls back to the key");
        assert!(reader.probe().is_ok());
        assert_eq!(reader.dedupe_key(cowork), "53c9fd03-e50a-4baa-aadd-cb1c75c60f07");
        assert_eq!(reader.dedupe_key(claude), claude.session_id);
        assert_eq!(reader.load(claude).unwrap().len(), 2);
        let stats = reader.load_stats(claude).unwrap().expect("priced");
        assert_eq!(stats.cost_usd.as_deref(), Some("0.717783"));
        assert_eq!(reader.load_stats(cowork).unwrap(), None, "no token data → None");
        // A 404 usage is advisory → None, not an error.
        let missing = TranscriptHandle {
            session_id: "codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373".into(),
            path: PathBuf::from("agentsview://x"),
            modified: Utc::now(),
            reader_name: "agentsview".into(),
            persona: None,
            channel: None,
        };
        assert_eq!(reader.load_stats(&missing).unwrap(), None);
        {
            let seen = seen.lock().unwrap();
            assert!(seen.iter().all(|(_, auth)| auth == "Bearer secret-1"), "{seen:?}");
            assert!(seen.iter().all(|(p, _)| p.starts_with("/api/v1/")), "every request is under /api/v1: {seen:?}");
            assert!(
                seen.iter().any(|(p, _)| p.starts_with(
                    "/api/v1/sessions?limit=500&active_since=2026-09-01T00:00:00Z&include_automated=true&include_one_shot=true&include_children=true"
                )),
                "{seen:?}"
            );
            assert!(
                seen.iter().any(|(p, _)| p == "/api/v1/sessions/a37ffc87-2799-4a09-830b-a92fde71d768/messages?from=0&limit=500&direction=asc"),
                "{seen:?}"
            );
            assert!(
                seen.iter().any(|(p, _)| p == "/api/v1/sessions/a37ffc87-2799-4a09-830b-a92fde71d768/usage?breakdown=true"),
                "{seen:?}"
            );
            assert!(seen.iter().any(|(p, _)| p == "/api/v1/machines"), "{seen:?}");
            assert!(seen.iter().any(|(p, _)| p.contains("cursor=CURSOR1")), "second page requested");
        }
        // A narrower window: the finished Claude session (ended 2026-09-11)
        // drops out; the codex session (ended 2026-09-16T00:00:19Z) stays;
        // the still-active cowork session (ended_at null, started
        // 2026-09-15) stays too — it is active now, whatever it started.
        let since = DateTime::parse_from_rfc3339("2026-09-16T00:00:00Z").unwrap().with_timezone(&Utc);
        let ids: Vec<String> = reader.discover(since).unwrap().into_iter().map(|h| h.session_id).collect();
        assert_eq!(
            ids,
            ["codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373", "cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07"],
            "{ids:?}"
        );
    }

    #[test]
    fn unreachable_daemon_is_a_reader_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("t");
        std::fs::write(&token_file, "x").unwrap();
        // Port from a listener we immediately drop → connection refused.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let reader = AgentsviewReader::new(&format!("http://127.0.0.1:{port}"), token_file, 7, Duration::from_secs(2)).unwrap();
        let err = reader.discover(Utc::now()).unwrap_err().to_string();
        assert!(err.contains("unreachable"), "{err}");
        assert!(!err.contains("Bearer"), "never echo the header: {err}");
        assert!(reader.probe().is_err());
    }
}
