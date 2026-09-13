//! Dispatch and allocation evidence for Codex-worker failures.
//!
//! This reader emits tool-error messages so the normal P3 filing and title
//! deduplication apply. It does not infer a trust prompt from a quiet session.

use crate::{
    error::JilogReviewError,
    reader::{Message, Reader, TranscriptHandle},
    util::expand_tilde,
};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufRead,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

pub struct WorkerSignalsReader {
    pub pool_dir: PathBuf,
    pub kata_bin: PathBuf,
    /// kata-dispatch hook state directory (the hook's own
    /// `KATA_DISPATCH_STATE_DIR`).
    pub state_dir: PathBuf,
    /// This machine's short hostname. `None` disables missing-hook
    /// detection: a dispatch recorded elsewhere cannot be judged here.
    pub host: Option<String>,
    /// The main Codex home (`CODEX_HOME`). Its `sessions` tree and every
    /// pool profile's are scanned for the rollout that confirms a first
    /// assistant turn; its `config.toml` carries the main seat's hook trust.
    pub codex_home: PathBuf,
    evidence: Mutex<Vec<(TranscriptHandle, Message)>>,
}

impl Default for WorkerSignalsReader {
    fn default() -> Self {
        Self {
            pool_dir: expand_tilde("~/.codex-pool"),
            kata_bin: "kata".into(),
            state_dir: std::env::var_os("KATA_DISPATCH_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| expand_tilde("~/.local/state/kata-dispatch")),
            host: local_host(),
            codex_home: expand_tilde("~/.codex"),
            evidence: Mutex::new(Vec::new()),
        }
    }
}

/// The short hostname, read the way kata-dispatch records it. An unreadable
/// or empty hostname yields `None`, which suppresses the host-scoped
/// missing-hook kind rather than guessing at an identity.
fn local_host() -> Option<String> {
    let output = Command::new("hostname").arg("-s").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

fn kata(bin: &std::path::Path, args: &[&str]) -> Result<Value, JilogReviewError> {
    let output = Command::new(bin).args(args).output()?;
    if !output.status.success() {
        return Err(JilogReviewError::Reader(format!(
            "worker-signals: kata exited {}",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| JilogReviewError::Reader(format!("worker-signals: invalid kata JSON: {e}")))
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn record(
    id: &str,
    kind: &str,
    detail: &str,
    at: DateTime<Utc>,
    path: PathBuf,
    seat: Option<&str>,
) -> (TranscriptHandle, Message) {
    let identity = format!("{id}:{kind}");
    (
        TranscriptHandle {
            session_id: identity,
            path,
            modified: at,
            reader_name: "worker-signals".into(),
            persona: None,
            channel: None,
        },
        Message {
            role: Some("tool".into()),
            name: Some(kind.into()),
            content: Some(
                serde_json::json!({"success": false, "error": format!("{id}: {detail}"), "seat": seat}),
            ),
        },
    )
}

/// A review comment must start with the recorded command, rather than quote
/// policy text. Only comments belonging to the current dispatch are eligible.
fn same_model_review(body: &str, harness: &str) -> bool {
    let Some(command) = body.lines().next().and_then(|s| s.strip_prefix("review: ")) else {
        return false;
    };
    let words: Vec<_> = command.split_whitespace().collect();
    if words.first() != Some(&"fresheyes") {
        return false;
    }
    let gpt = words.contains(&"--gpt");
    let claude = words.contains(&"--claude");
    matches!(
        (harness, gpt, claude),
        ("codex", true, false) | ("claude", false, true)
    )
}

fn dispatch_records(full: &Value, since: DateTime<Utc>) -> Vec<(TranscriptHandle, Message)> {
    let issue = &full["issue"];
    let dispatch = &issue["metadata"]["dispatch"];
    let Some(id) = dispatch["id"].as_str() else {
        return vec![];
    };
    let Some(start) = timestamp(&dispatch["dispatched_at"]) else {
        return vec![];
    };
    let harness = dispatch["harness"].as_str().unwrap_or("");
    let seat = dispatch["seat"].as_str();
    let path = PathBuf::from(format!("kata:{}", issue["uid"].as_str().unwrap_or(id)));
    let mut out = Vec::new();
    let kickoff = &issue["metadata"]["kickoff"];
    if harness == "codex"
        && kickoff["id"].as_str() == Some(id)
        && kickoff["state"] == "failed"
        && kickoff["detail"]
            .as_str()
            .is_some_and(|s| s.starts_with("dialog:dir-trust:"))
    {
        if let Some(at) = timestamp(&kickoff["at"]).filter(|t| *t >= since && *t >= start) {
            out.push(record(
                id,
                "codex_trust_prompt",
                "kickoff stopped at directory-trust prompt",
                at,
                path.clone(),
                seat,
            ));
        }
    }
    for comment in full["comments"].as_array().into_iter().flatten() {
        let Some(at) = timestamp(&comment["created_at"]).filter(|t| *t >= since && *t >= start)
        else {
            continue;
        };
        if same_model_review(comment["body"].as_str().unwrap_or(""), harness) {
            out.push(record(
                id,
                "same_model_review",
                "review used builder model family",
                at,
                path.clone(),
                seat,
            ));
            break;
        }
    }
    out
}

fn allocation_records(
    log: &str,
    path: &std::path::Path,
    seats_exist: bool,
    since: DateTime<Utc>,
) -> Vec<(TranscriptHandle, Message)> {
    if !seats_exist {
        return vec![];
    }
    let mut groups = std::collections::BTreeMap::<(String, String), (DateTime<Utc>, usize)>::new();
    for line in log.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() < 4 || fields[2] != "exec" || fields[3] != "main" {
            continue;
        }
        // The launcher writes host-local timestamps without an offset.
        let at = DateTime::parse_from_rfc3339(fields[0])
            .ok()
            .map(|t| t.with_timezone(&Utc))
            .or_else(|| {
                NaiveDateTime::parse_from_str(fields[0], "%Y-%m-%dT%H:%M:%S")
                    .ok()
                    .and_then(|t| Local.from_local_datetime(&t).single())
                    .map(|t| t.with_timezone(&Utc))
            });
        let Some(at) = at else {
            tracing::warn!(
                "worker-signals: invalid or ambiguous allocation timestamp {}; row skipped",
                fields[0]
            );
            continue;
        };
        if at < since {
            continue;
        }
        let date = fields[0].get(..10).unwrap_or(fields[0]).to_string();
        let entry = groups
            .entry((fields[1].to_owned(), date))
            .or_insert((at, 0));
        entry.0 = entry.0.max(at);
        entry.1 += 1;
    }
    groups.into_iter().map(|((host, date), (at, count))| {
        // The requested main-usage signal includes scored selections. The
        // log has no dispatch ID, so preserve host/day and the source path.
        let id = format!("allocation:{host}:{date}");
        record(&id, "codex_fallback_main",
            &format!("main selected while pool profiles exist; {count} exec row(s); last timestamp {}; source {}; allocation log has no dispatch id", at.to_rfc3339(), path.display()),
            at, path.to_owned(), Some("main"))
    }).collect()
}

// ---------------------------------------------------------------------------
// Missing hook state (jilog#4nd2)
// ---------------------------------------------------------------------------
//
// A dispatched codex pane whose kata-dispatch hook never wrote its state file
// is invisible to the liveness watcher and to `--send-only`. The absence of
// the file proves nothing on its own: the pane may have been torn down, the
// pane number reused by a later dispatch, the dispatch recorded on another
// host, or the seat's codex hooks left untrusted. This kind is emitted only
// when the whole identity is confirmed on THIS host and the rollout shows the
// worker actually produced a first assistant turn.

/// kata-dispatch-hook's filename sanitiser: everything outside
/// `[A-Za-z0-9._-]` becomes `-`.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The pane number the hook writes: a tmux pane id without its `%`.
fn pane_number(pane: &str) -> Option<String> {
    let number = sanitize(pane.strip_prefix('%').unwrap_or(pane));
    (!number.is_empty()).then_some(number)
}

/// Hook state file names for one pane identity. The hook dual-writes the
/// pre-socket name on the default server (jibot-code#w4ae), so either file
/// proves the hook ran. The first name is the current contract.
fn hook_state_names(reference: &str, socket: &str, pane: &str) -> Vec<String> {
    let slug = sanitize(reference);
    let mut names = vec![format!("{slug}-{}-{pane}.json", sanitize(socket))];
    if socket == "default" {
        names.push(format!("{slug}-{pane}.json"));
    }
    names
}

/// True when any terminal marker (`<stem>.ended.<session>`) exists for these
/// state file names — the pane reached a codex SessionEnd (jibot-code#vq34).
fn ended_marker_present(names: &[String], files: &BTreeSet<String>) -> bool {
    names.iter().any(|name| {
        let prefix = format!("{}.ended.", name.trim_end_matches(".json"));
        files.iter().any(|f| f.starts_with(&prefix))
    })
}

/// A codex seat trusts its hooks once the harness has stored a hash for them.
/// Without a stored hash the pane stops at the hook-review dialog and writes
/// nothing, which is a different, already-visible condition.
fn codex_hooks_trusted(config_toml: &str) -> bool {
    config_toml
        .lines()
        .any(|line| line.trim_start().starts_with("trusted_hash"))
}

/// The newest dispatch recorded for each `(host, socket, pane)` identity,
/// across every listed issue. A pane whose newest dispatch is some other id
/// was reused, and this dispatch's missing file says nothing about the hook.
fn newest_pane_dispatches(
    listed: &Value,
) -> BTreeMap<(String, String, String), (DateTime<Utc>, String)> {
    let mut newest = BTreeMap::new();
    for issue in listed["issues"].as_array().into_iter().flatten() {
        let dispatch = &issue["metadata"]["dispatch"];
        let (Some(id), Some(host), Some(pane)) = (
            dispatch["id"].as_str(),
            dispatch["host"].as_str(),
            dispatch["pane"].as_str().and_then(pane_number),
        ) else {
            continue;
        };
        let Some(at) = timestamp(&dispatch["dispatched_at"]) else {
            continue;
        };
        let socket = dispatch["tmux_socket"]
            .as_str()
            .unwrap_or("default")
            .to_owned();
        let key = (host.to_owned(), socket, pane);
        let entry = newest.entry(key).or_insert_with(|| (at, id.to_owned()));
        if (at, id) > (entry.0, entry.1.as_str()) {
            *entry = (at, id.to_owned());
        }
    }
    newest
}

/// One Codex rollout: where it ran and when it started.
struct Rollout {
    path: PathBuf,
    cwd: String,
    started: DateTime<Utc>,
}

/// The first line of a rollout is its `session_meta`, carrying the working
/// directory and start time. Anything else is not a usable rollout here.
fn rollout_meta(path: &Path) -> Option<Rollout> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value["type"].as_str() != Some("session_meta") {
        return None;
    }
    let cwd = value["payload"]["cwd"].as_str()?.to_owned();
    let started =
        timestamp(&value["payload"]["timestamp"]).or_else(|| timestamp(&value["timestamp"]))?;
    Some(Rollout {
        path: path.to_owned(),
        cwd,
        started,
    })
}

/// Timestamp of the first assistant message in a rollout, if it produced one.
fn first_assistant_turn(path: &Path) -> Option<DateTime<Utc>> {
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["type"].as_str() != Some("response_item") {
            continue;
        }
        let payload = &value["payload"];
        if payload["type"].as_str() != Some("message")
            || payload["role"].as_str() != Some("assistant")
        {
            continue;
        }
        if let Some(at) = timestamp(&value["timestamp"]) {
            return Some(at);
        }
    }
    None
}

/// Rollouts touched since the scan window opened, grouped by working
/// directory. Only the first line of each file is read here.
fn rollout_index(roots: &[PathBuf], since: DateTime<Utc>) -> BTreeMap<String, Vec<Rollout>> {
    let mut index: BTreeMap<String, Vec<Rollout>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let pattern = format!(
            "{}/**/rollout-*.jsonl",
            glob::Pattern::escape(&root.to_string_lossy())
        );
        let Ok(entries) = glob::glob(&pattern) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(canonical) = std::fs::canonicalize(&entry) else {
                continue;
            };
            if !seen.insert(canonical) {
                continue;
            }
            let fresh = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| Utc.timestamp_opt(d.as_secs() as i64, 0).single())
                .map_or(true, |modified| modified >= since);
            if !fresh {
                continue;
            }
            if let Some(rollout) = rollout_meta(&entry) {
                index.entry(rollout.cwd.clone()).or_default().push(rollout);
            }
        }
    }
    index
}

/// Everything the missing-hook rule needs that is not in the issue itself.
struct HookEvidence<'a> {
    host: &'a str,
    /// File names present in the hook state directory.
    state_files: &'a BTreeSet<String>,
    newest_pane: &'a BTreeMap<(String, String, String), (DateTime<Utc>, String)>,
    rollouts: &'a BTreeMap<String, Vec<Rollout>>,
    /// Whether the dispatch's codex seat has trusted its hooks.
    hooks_trusted: bool,
}

/// The missing-hook-state record for one issue. `reference` is the
/// project-qualified id kata-dispatch was invoked with — the hook derives its
/// filename slug from it, and `kata show` does not echo it back. The record is
/// produced only when every identity condition holds. Any unconfirmed condition returns `None` — silence is the correct
/// answer for evidence this reader cannot establish.
fn missing_hook_state_record(
    full: &Value,
    reference: &str,
    since: DateTime<Utc>,
    evidence: &HookEvidence,
) -> Option<(TranscriptHandle, Message)> {
    let issue = &full["issue"];
    let dispatch = &issue["metadata"]["dispatch"];
    if dispatch["harness"].as_str() != Some("codex") {
        return None;
    }
    let id = dispatch["id"].as_str()?;
    let start = timestamp(&dispatch["dispatched_at"])?;
    // Another host's hook state is not readable from here.
    if dispatch["host"].as_str()? != evidence.host {
        return None;
    }
    let pane = pane_number(dispatch["pane"].as_str()?)?;
    let socket = dispatch["tmux_socket"].as_str().unwrap_or("default");
    let worktree = dispatch["worktree"].as_str()?;

    // Pane reuse: a later dispatch owns this pane number now.
    let key = (evidence.host.to_owned(), socket.to_owned(), pane.clone());
    if evidence
        .newest_pane
        .get(&key)
        .map(|(_, newest)| newest.as_str())
        != Some(id)
    {
        return None;
    }
    let names = hook_state_names(reference, socket, &pane);
    if names.iter().any(|n| evidence.state_files.contains(n)) {
        return None;
    }
    // Teardown: a terminal marker, or a worktree that no longer exists.
    if ended_marker_present(&names, evidence.state_files) || !Path::new(worktree).is_dir() {
        return None;
    }
    if !evidence.hooks_trusted {
        return None;
    }
    // The worker must have actually run: a rollout in the dispatch worktree,
    // started at or after the dispatch, with a first assistant turn.
    let at = evidence
        .rollouts
        .get(worktree)?
        .iter()
        .filter(|r| r.started >= start)
        .filter_map(|r| first_assistant_turn(&r.path).map(|at| (at, &r.path)))
        .filter(|(at, _)| *at >= start)
        .min_by(|a, b| a.0.cmp(&b.0));
    let (at, rollout) = at?;
    if at < since {
        return None;
    }
    Some(record(
        id,
        "codex_missing_hook_state",
        &format!(
            "no kata-dispatch hook state for pane %{pane} on {} (socket {socket}) after the first assistant turn at {}; expected {}; rollout {}",
            evidence.host,
            at.to_rfc3339(),
            names.join(" or "),
            rollout.display()
        ),
        at,
        PathBuf::from(format!("kata:{}", issue["uid"].as_str().unwrap_or(id))),
        dispatch["seat"].as_str(),
    ))
}

impl WorkerSignalsReader {
    /// Codex seat homes: the main home plus every pool profile.
    fn codex_home_for(&self, seat: Option<&str>) -> PathBuf {
        match seat {
            Some(seat) if seat != "main" => self.pool_dir.join("profiles").join(seat),
            _ => self.codex_home.clone(),
        }
    }

    fn rollout_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.codex_home.join("sessions")];
        if let Ok(entries) = std::fs::read_dir(self.pool_dir.join("profiles")) {
            for entry in entries.flatten() {
                roots.push(entry.path().join("sessions"));
            }
        }
        roots.sort();
        roots.dedup();
        roots
    }

    /// Whether the seat's Codex config records a trusted hook hash. An
    /// unreadable config reads as untrusted, which suppresses the kind.
    fn hooks_trusted(&self, seat: Option<&str>) -> bool {
        std::fs::read_to_string(self.codex_home_for(seat).join("config.toml"))
            .map(|c| codex_hooks_trusted(&c))
            .unwrap_or(false)
    }

    /// File names in the hook state directory. `None` when the directory
    /// cannot be read: absence of evidence is not evidence of a missing hook.
    fn state_files(&self) -> Option<BTreeSet<String>> {
        match std::fs::read_dir(&self.state_dir) {
            Ok(entries) => Some(
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect(),
            ),
            Err(error) => {
                tracing::warn!(
                    "worker-signals: cannot read hook state dir {}: {error}; missing-hook detection skipped",
                    self.state_dir.display()
                );
                None
            }
        }
    }
}

impl Reader for WorkerSignalsReader {
    fn name(&self) -> &str {
        "worker-signals"
    }
    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let listed_result = kata(
            &self.kata_bin,
            &[
                "list", "--all", "--status", "all", "--meta", "dispatch", "--limit", "0", "--json",
            ],
        );
        let listed = match listed_result {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("worker-signals: {error}; continuing with allocation evidence");
                serde_json::json!({"issues": []})
            }
        };
        if !listed["issues"].is_array() {
            tracing::warn!(
                "worker-signals: missing issues array; continuing with allocation evidence"
            );
        }
        let mut records = Vec::new();
        let newest_pane = newest_pane_dispatches(&listed);
        let state_files = self.state_files();
        let mut rollouts: Option<BTreeMap<String, Vec<Rollout>>> = None;
        let mut trust: BTreeMap<String, bool> = BTreeMap::new();
        for issue in listed["issues"].as_array().into_iter().flatten() {
            // Kata updates updated_at when a comment is appended. Keep
            // missing timestamps eligible rather than silently losing data.
            if timestamp(&issue["updated_at"]).is_some_and(|t| t < since)
                && timestamp(&issue["metadata"]["dispatch"]["dispatched_at"])
                    .map_or(true, |t| t < since)
                && timestamp(&issue["metadata"]["kickoff"]["at"]).map_or(true, |t| t < since)
            {
                continue;
            }
            let harness = issue["metadata"]["dispatch"]["harness"]
                .as_str()
                .unwrap_or("");
            if !matches!(harness, "codex" | "claude") {
                continue;
            }
            let Some(qualified) = issue["qualified_id"].as_str() else {
                tracing::warn!("worker-signals: missing qualified issue reference; skipping issue");
                continue;
            };
            match kata(&self.kata_bin, &["show", qualified, "--json"]) {
                Ok(full) => {
                    records.extend(dispatch_records(&full, since));
                    if let (Some(host), Some(state_files)) = (self.host.as_deref(), &state_files) {
                        let seat = full["issue"]["metadata"]["dispatch"]["seat"].as_str();
                        let hooks_trusted = *trust
                            .entry(seat.unwrap_or("main").to_owned())
                            .or_insert_with(|| self.hooks_trusted(seat));
                        let rollouts = rollouts
                            .get_or_insert_with(|| rollout_index(&self.rollout_roots(), since));
                        let evidence = HookEvidence {
                            host,
                            state_files,
                            newest_pane: &newest_pane,
                            rollouts,
                            hooks_trusted,
                        };
                        records.extend(missing_hook_state_record(
                            &full, qualified, since, &evidence,
                        ));
                    }
                }
                Err(error) => tracing::warn!(
                    "worker-signals: {qualified}: {error}; continuing with other evidence"
                ),
            }
        }
        let log_path = self.pool_dir.join("log/allocation.log");
        let seats_exist = match std::fs::read_dir(self.pool_dir.join("profiles")) {
            Ok(mut entries) => entries.any(|e| e.is_ok_and(|e| e.path().is_dir())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                tracing::warn!("worker-signals: cannot inspect pool profiles: {e}");
                false
            }
        };
        match std::fs::read_to_string(&log_path) {
            Ok(log) => records.extend(allocation_records(&log, &log_path, seats_exist, since)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("worker-signals: cannot read allocation log: {e}"),
        }
        records.sort_by(|a, b| a.0.session_id.cmp(&b.0.session_id));
        records.dedup_by(|a, b| a.0.session_id == b.0.session_id);
        let handles = records.iter().map(|r| r.0.clone()).collect();
        *self.evidence.lock().unwrap() = records;
        Ok(handles)
    }
    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        Ok(self
            .evidence
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.0.session_id == handle.session_id)
            .map(|r| r.1.clone())
            .collect())
    }
    fn seat(&self, handle: &TranscriptHandle) -> Option<String> {
        self.evidence
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.0.session_id == handle.session_id)
            .and_then(|r| r.1.content.as_ref())
            .and_then(|v| v.get("seat"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn review_command_requires_explicit_matching_provider() {
        assert!(same_model_review("review: fresheyes --gpt", "codex"));
        assert!(same_model_review("review: fresheyes --claude", "claude"));
        for body in [
            "Use review: fresheyes --gpt",
            "review: fresheyes --claude",
            "review: fresheyes --gpt --claude",
            "review: fresheyes --gpt-extra",
        ] {
            assert!(!same_model_review(body, "codex"));
        }
    }
    #[test]
    fn records_require_current_dispatch_and_fresh_evidence() {
        let mut full = json!({"issue": {"uid":"issue", "metadata": {
            "dispatch":{"id":"dispatch-1", "harness":"codex", "seat":"codex-01", "dispatched_at":"2026-09-13T00:00:00Z"},
            "kickoff":{"id":"dispatch-1", "state":"failed", "detail":"dialog:dir-trust: wait", "at":"2026-09-13T00:01:00Z"}
        }}, "comments":[{"body":"review: fresheyes --gpt", "created_at":"2026-09-13T00:02:00Z"}]});
        let since = timestamp(&json!("2026-09-12T00:00:00Z")).unwrap();
        let records = dispatch_records(&full, since);
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].1.content.as_ref().unwrap()["seat"].as_str(),
            Some("codex-01")
        );
        full["issue"]["metadata"]["kickoff"]["id"] = json!("old-dispatch");
        full["comments"][0]["created_at"] = json!("2026-09-12T23:59:59Z");
        assert!(dispatch_records(&full, since).is_empty());
    }
    #[test]
    fn allocation_filters_and_preserves_identity() {
        let since = timestamp(&json!("2026-09-12T00:00:00Z")).unwrap();
        let log = "2026-09-13T00:00:00Z\thost\texec\tmain\n2026-09-13T00:00:01Z\thost\tpick\tmain\n2026-09-13T00:00:02Z\thost\texec\tcodex-01\ninvalid\thost\texec\tmain\n";
        assert!(allocation_records(log, std::path::Path::new("log"), false, since).is_empty());
        let records = allocation_records(log, std::path::Path::new("log"), true, since);
        assert_eq!(records.len(), 1);
        let twice = allocation_records(
            &format!("{log}{log}"),
            std::path::Path::new("log"),
            true,
            since,
        );
        assert_eq!(twice.len(), 1);
        assert_eq!(twice[0].0.session_id, records[0].0.session_id);
        assert_eq!(
            records[0].0.session_id,
            "allocation:host:2026-09-13:codex_fallback_main"
        );
    }

    fn hook_fixture(
        tree: &std::path::Path,
        started: &str,
        assistant: Option<&str>,
    ) -> (Value, BTreeMap<String, Vec<Rollout>>, DateTime<Utc>) {
        let worktree = tree.join("worktree");
        let day = tree.join(".codex/sessions/2026/09/13");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&day).unwrap();
        let mut lines = vec![json!({
            "timestamp": started,
            "type": "session_meta",
            "payload": {"session_id": "s1", "cwd": worktree, "timestamp": started}
        })
        .to_string()];
        if let Some(at) = assistant {
            lines.push(
                json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_started"}})
                    .to_string(),
            );
            lines.push(
                json!({"timestamp": at, "type": "response_item", "payload": {
                "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "on it"}]}})
                .to_string(),
            );
        }
        std::fs::write(
            day.join("rollout-2026-09-13T00-00-30-s1.jsonl"),
            lines.join("\n"),
        )
        .unwrap();
        let since = timestamp(&json!("2026-09-12T00:00:00Z")).unwrap();
        let rollouts = rollout_index(&[tree.join(".codex/sessions")], since);
        let full = json!({"issue": {"uid": "issue-uid", "qualified_id": "jilog#4nd2", "metadata": {
            "dispatch": {"id": "dispatch-1", "harness": "codex", "seat": "codex-01",
                "host": "macazbd", "pane": "%542", "tmux_socket": "default",
                "worktree": worktree, "dispatched_at": "2026-09-13T00:00:00Z"}
        }}});
        (full, rollouts, since)
    }

    fn newest_for(full: &Value) -> BTreeMap<(String, String, String), (DateTime<Utc>, String)> {
        newest_pane_dispatches(&json!({"issues": [full["issue"].clone()]}))
    }

    #[test]
    fn hook_state_names_follow_the_socket_qualified_contract() {
        assert_eq!(pane_number("%542").as_deref(), Some("542"));
        assert_eq!(pane_number("%").as_deref(), None);
        assert_eq!(
            hook_state_names("jilog#4nd2", "default", "542"),
            vec!["jilog-4nd2-default-542.json", "jilog-4nd2-542.json"]
        );
        assert_eq!(
            hook_state_names("jilog#4nd2", "kwt", "542"),
            vec!["jilog-4nd2-kwt-542.json"]
        );
        assert!(codex_hooks_trusted(
            "[hooks.state.\"x\"]\ntrusted_hash = \"sha256:a\"\n"
        ));
        assert!(!codex_hooks_trusted("[hooks.state]\n"));
    }

    #[test]
    fn missing_hook_state_needs_a_first_assistant_turn_in_this_dispatch() {
        let tree = tempfile::tempdir().unwrap();
        let (full, rollouts, since) = hook_fixture(
            tree.path(),
            "2026-09-13T00:00:30Z",
            Some("2026-09-13T00:01:00Z"),
        );
        let newest = newest_for(&full);
        let state_files = BTreeSet::new();
        let evidence = HookEvidence {
            host: "macazbd",
            state_files: &state_files,
            newest_pane: &newest,
            rollouts: &rollouts,
            hooks_trusted: true,
        };
        let (handle, message) = missing_hook_state_record(&full, "jilog#4nd2", since, &evidence)
            .expect("confirmed identity files");
        assert_eq!(handle.session_id, "dispatch-1:codex_missing_hook_state");
        assert_eq!(message.name.as_deref(), Some("codex_missing_hook_state"));
        let content = message.content.as_ref().unwrap();
        assert_eq!(content["seat"].as_str(), Some("codex-01"));
        let detail = content["error"].as_str().unwrap();
        assert!(detail.contains("jilog-4nd2-default-542.json"), "{detail}");
        assert!(detail.contains("%542"), "{detail}");
        assert_eq!(
            handle.modified,
            timestamp(&json!("2026-09-13T00:01:00Z")).unwrap()
        );

        // A rollout with no assistant turn proves nothing about the hook.
        let quiet = tempfile::tempdir().unwrap();
        let (quiet_full, quiet_rollouts, _) =
            hook_fixture(quiet.path(), "2026-09-13T00:00:30Z", None);
        let quiet_newest = newest_for(&quiet_full);
        assert!(missing_hook_state_record(
            &quiet_full,
            "jilog#4nd2",
            since,
            &HookEvidence {
                rollouts: &quiet_rollouts,
                newest_pane: &quiet_newest,
                ..evidence
            }
        )
        .is_none());

        // A rollout that predates the dispatch belongs to earlier work.
        let stale = tempfile::tempdir().unwrap();
        let (stale_full, stale_rollouts, _) = hook_fixture(
            stale.path(),
            "2026-09-12T23:00:00Z",
            Some("2026-09-12T23:01:00Z"),
        );
        let stale_newest = newest_for(&stale_full);
        assert!(missing_hook_state_record(
            &stale_full,
            "jilog#4nd2",
            since,
            &HookEvidence {
                rollouts: &stale_rollouts,
                newest_pane: &stale_newest,
                ..evidence
            }
        )
        .is_none());
    }

    #[test]
    fn missing_hook_state_is_silent_without_a_confirmed_live_identity() {
        let tree = tempfile::tempdir().unwrap();
        let (full, rollouts, since) = hook_fixture(
            tree.path(),
            "2026-09-13T00:00:30Z",
            Some("2026-09-13T00:01:00Z"),
        );
        let newest = newest_for(&full);
        let empty = BTreeSet::new();
        let base = HookEvidence {
            host: "macazbd",
            state_files: &empty,
            newest_pane: &newest,
            rollouts: &rollouts,
            hooks_trusted: true,
        };
        assert!(missing_hook_state_record(&full, "jilog#4nd2", since, &base).is_some());

        // Valid hooks: either the socket-qualified file or its default-server
        // transition copy is proof the hook ran.
        for name in ["jilog-4nd2-default-542.json", "jilog-4nd2-542.json"] {
            let present = BTreeSet::from([name.to_string()]);
            assert!(
                missing_hook_state_record(
                    &full,
                    "jilog#4nd2",
                    since,
                    &HookEvidence {
                        state_files: &present,
                        ..base
                    }
                )
                .is_none(),
                "{name} must suppress the kind"
            );
        }

        // Teardown: a terminal marker, or a worktree that is gone.
        let ended = BTreeSet::from(["jilog-4nd2-default-542.ended.s1".to_string()]);
        assert!(missing_hook_state_record(
            &full,
            "jilog#4nd2",
            since,
            &HookEvidence {
                state_files: &ended,
                ..base
            }
        )
        .is_none());
        let mut removed = full.clone();
        removed["issue"]["metadata"]["dispatch"]["worktree"] =
            json!(tree.path().join("gone").to_string_lossy());
        assert!(missing_hook_state_record(&removed, "jilog#4nd2", since, &base).is_none());

        // Remote dispatch: another host's state directory is not readable here.
        assert!(missing_hook_state_record(
            &full,
            "jilog#4nd2",
            since,
            &HookEvidence {
                host: "joimba",
                ..base
            }
        )
        .is_none());

        // Pane reuse: a later dispatch owns pane %542 now.
        let mut later = full["issue"].clone();
        later["metadata"]["dispatch"]["id"] = json!("dispatch-2");
        later["metadata"]["dispatch"]["dispatched_at"] = json!("2026-09-13T01:00:00Z");
        let reused = newest_pane_dispatches(&json!({"issues": [full["issue"].clone(), later]}));
        assert_eq!(reused.len(), 1);
        assert!(missing_hook_state_record(
            &full,
            "jilog#4nd2",
            since,
            &HookEvidence {
                newest_pane: &reused,
                ..base
            }
        )
        .is_none());

        // Untrusted hooks: the pane stops at the hook-review dialog instead.
        assert!(missing_hook_state_record(
            &full,
            "jilog#4nd2",
            since,
            &HookEvidence {
                hooks_trusted: false,
                ..base
            }
        )
        .is_none());

        // Claude panes install hooks by a different path; out of scope here.
        let mut claude = full.clone();
        claude["issue"]["metadata"]["dispatch"]["harness"] = json!("claude");
        assert!(missing_hook_state_record(&claude, "jilog#4nd2", since, &base).is_none());

        // Evidence older than the scan window is not refiled.
        let later_since = timestamp(&json!("2026-09-13T02:00:00Z")).unwrap();
        assert!(missing_hook_state_record(&full, "jilog#4nd2", later_since, &base).is_none());
    }
}
