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
    time::{Duration, Instant},
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

/// The pane number the hook writes: a tmux pane id without its `%`. Anything
/// that is not a run of digits is not a pane — kata-dispatch writes the
/// literal `-` as a placeholder in a phase-1 record that never got a pane.
fn pane_number(pane: &str) -> Option<String> {
    let number = pane.strip_prefix('%')?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then(|| number.to_owned())
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

/// The `[hooks.state]` key Codex stores trust under for the dispatch-state
/// SessionStart hook: `<resolved hooks.json>:session_start:<group>:<hook>`.
/// The command must be exactly `<python…> <runner> dispatch-state`, the way
/// the installer writes it — `codex-parity-check` matches the same shape, and
/// a substring match would accept an impostor that never runs the bridge.
fn dispatch_state_trust_key(hooks_json: &Path) -> Option<String> {
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(hooks_json).ok()?).ok()?;
    let resolved = std::fs::canonicalize(hooks_json).unwrap_or_else(|_| hooks_json.to_owned());
    for (group, entry) in doc["hooks"]["SessionStart"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (index, hook) in entry["hooks"].as_array().into_iter().flatten().enumerate() {
            if hook["type"].as_str() != Some("command") {
                continue;
            }
            let argv: Vec<_> = hook["command"].as_str()?.split_whitespace().collect();
            let python = argv
                .first()
                .and_then(|a| Path::new(a).file_name())
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("python"));
            if argv.len() == 3 && argv[2] == "dispatch-state" && python {
                return Some(format!(
                    "{}:session_start:{group}:{index}",
                    resolved.display()
                ));
            }
        }
    }
    None
}

/// Whether this seat has trusted the dispatch-state hook itself. Trust is
/// stored per hook entry, so a hash for the sibling `startup` hook — or for
/// any other event — is not this hook's trust, and Codex would still stop the
/// pane at the hook-review dialog, a different and already-visible condition.
fn codex_hooks_trusted(config_toml: &str, key: &str) -> bool {
    let Ok(doc) = config_toml.parse::<toml::Value>() else {
        return false;
    };
    doc.get("hooks")
        .and_then(|h| h.get("state"))
        .and_then(|s| s.get(key))
        .and_then(|entry| entry.get("trusted_hash"))
        .and_then(toml::Value::as_str)
        .is_some_and(|hash| !hash.is_empty())
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

/// One Codex rollout: which session it is, where it ran, when it started, and
/// how it was launched.
struct Rollout {
    path: PathBuf,
    session_id: Option<String>,
    cwd: String,
    started: DateTime<Utc>,
    /// `session_meta.payload.originator`; `codex_exec` marks a non-interactive
    /// run (a review pass, a scripted call) that never carries dispatch markers.
    originator: Option<String>,
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
    let payload = &value["payload"];
    let cwd = payload["cwd"].as_str()?.to_owned();
    let started = timestamp(&payload["timestamp"]).or_else(|| timestamp(&value["timestamp"]))?;
    Some(Rollout {
        path: path.to_owned(),
        session_id: payload["session_id"]
            .as_str()
            .or_else(|| payload["id"].as_str())
            .map(str::to_owned),
        cwd,
        started,
        originator: payload["originator"].as_str().map(str::to_owned),
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
                index
                    .entry(canonical_key(&rollout.cwd))
                    .or_default()
                    .push(rollout);
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
    /// Live probes, run only for a candidate that has passed every other
    /// condition. Both touch the world, so neither runs per issue.
    probe: &'a dyn HookProbe,
}

/// The two checks that must be made against the world as it is now, not
/// against the snapshot the scan opened with.
trait HookProbe {
    /// The pane's current directory on the recorded tmux server — the liveness
    /// watcher's own probe. `None` when the pane is gone or tmux cannot answer.
    fn pane_cwd(&self, socket: &str, pane: &str) -> Option<String>;
    /// Whether any of these hook state files, or a terminal marker for one,
    /// exists right now. A scan that opened before the pane's SessionStart
    /// would otherwise file a finding the hook has since disproved.
    fn state_present(&self, names: &[String]) -> bool;
}

/// The index key for a working directory: its resolved path where that can be
/// read, so a symlinked component cannot spell the same directory two ways.
fn canonical_key(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned())
}

/// Two paths name the same directory when they resolve to the same file. tmux
/// reports the kernel-physical path while kata records the as-written one, so
/// a symlinked component must not read as "the pane left the worktree".
fn same_directory(left: &str, right: &str) -> bool {
    match (
        std::fs::canonicalize(left).ok(),
        std::fs::canonicalize(right).ok(),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => left == right,
    }
}

/// The missing-hook-state record for one issue. `reference` is the canonical
/// `project#id` — what kata-dispatch qualifies a bare ref to before exporting
/// `KATA_DISPATCH_REF`, what the hook slugs its filename from, and what
/// `kata list` reports as `qualified_id`; `kata show` does not echo it back.
/// The record is produced only when every identity condition holds. Any unconfirmed condition returns `None` — silence is the correct
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
    // A closed issue is a finished dispatch, and the liveness watcher removes
    // the hook state file and every marker on its clean disarm — so absence
    // there is the normal end of a dispatch that worked, not a hook failure.
    if issue["status"].as_str() != Some("open") {
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
    // The worker must have actually run. Prefer the session kata-dispatch
    // bound at kickoff: a rescue, a `codex resume`, or a review pass in the
    // same worktree is a different session and says nothing about this pane.
    let kickoff = &issue["metadata"]["kickoff"];
    let bound = (kickoff["id"].as_str() == Some(id))
        .then(|| kickoff["session_observed"].as_str())
        .flatten()
        .filter(|s| *s != "-");
    let candidates = evidence
        .rollouts
        .get(&canonical_key(worktree))?
        .iter()
        .filter(|r| {
            match bound {
                // Without a bound session, only an interactive rollout can be this
                // pane: `codex exec` never carries the dispatch markers.
                None => r.started >= start && r.originator.as_deref() != Some("codex_exec"),
                Some(session) => r.session_id.as_deref() == Some(session),
            }
        });
    let (at, rollout) = candidates
        .filter_map(|r| first_assistant_turn(&r.path).map(|at| (at, &r.path)))
        .filter(|(at, _)| *at >= start)
        .min_by(|a, b| a.0.cmp(&b.0))?;
    if at < since {
        return None;
    }
    // Last, because it spawns a process: the pane must still be sitting in the
    // dispatch worktree. A pane that is gone leaves no live identity to
    // confirm, and its silence has explanations this reader cannot rule out.
    if !evidence
        .probe
        .pane_cwd(socket, &pane)
        .is_some_and(|cwd| same_directory(&cwd, worktree))
    {
        return None;
    }
    // The snapshot this scan opened with may predate the pane's SessionStart.
    if evidence.probe.state_present(&names) {
        return None;
    }
    Some(record(
        id,
        "codex_missing_hook_state",
        &format!(
            "no kata-dispatch hook state for live pane %{pane} on {} (socket {socket}) after the first assistant turn at {}; expected {}; session {}; rollout {}",
            evidence.host,
            at.to_rfc3339(),
            names.join(" or "),
            bound.unwrap_or("unbound"),
            rollout.display()
        ),
        at,
        PathBuf::from(format!("kata:{}", issue["uid"].as_str().unwrap_or(id))),
        dispatch["seat"].as_str(),
    ))
}

/// Run a command, killing it if it outlives `limit`. An unattended nightly
/// must not be held open by an unresponsive tmux server; a timeout reads as
/// "could not confirm", which suppresses the kind.
fn run_bounded(mut command: Command, limit: Duration) -> Option<std::process::Output> {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("worker-signals: pane probe timed out after {limit:?}");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

/// The live probe: tmux for the pane, the hook state directory for the files.
struct LiveHookProbe {
    state_dir: PathBuf,
}

impl HookProbe for LiveHookProbe {
    fn pane_cwd(&self, socket: &str, pane: &str) -> Option<String> {
        let mut command = Command::new("tmux");
        command.args([
            "-L",
            socket,
            "display-message",
            "-p",
            "-t",
            &format!("%{pane}"),
            // A pane kept by remain-on-exit still answers with its last
            // directory, so ask whether it is dead in the same breath.
            "#{pane_dead}\n#{pane_current_path}",
        ]);
        let output = run_bounded(command, Duration::from_secs(15))?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        if lines.next()?.trim() != "0" {
            return None;
        }
        let cwd = lines.next()?.trim().to_owned();
        (!cwd.is_empty()).then_some(cwd)
    }

    fn state_present(&self, names: &[String]) -> bool {
        // A directory this pass cannot read is "unknown", and unknown
        // suppresses: the documented rule is that unreadable hook state never
        // produces a finding.
        let Ok(entries) = std::fs::read_dir(&self.state_dir) else {
            return true;
        };
        let mut present = BTreeSet::new();
        for entry in entries {
            let Ok(entry) = entry else { return true };
            if let Some(name) = entry.file_name().to_str() {
                present.insert(name.to_owned());
            }
        }
        names.iter().any(|name| {
            present.contains(name) || {
                let prefix = format!("{}.ended.", name.trim_end_matches(".json"));
                present.iter().any(|f| f.starts_with(&prefix))
            }
        })
    }
}

impl WorkerSignalsReader {
    /// Codex seat homes: the main home plus every pool profile.
    fn codex_home_for(&self, seat: Option<&str>) -> PathBuf {
        match seat {
            // `-` is kata-dispatch's placeholder for "no pool seat": the pane
            // runs under the main CODEX_HOME, like an absent seat.
            Some(seat) if seat != "main" && seat != "-" => {
                self.pool_dir.join("profiles").join(seat)
            }
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

    /// Whether the seat has trusted its dispatch-state hook. An unreadable
    /// config or hooks.json reads as untrusted, which suppresses the kind.
    fn hooks_trusted(&self, seat: Option<&str>) -> bool {
        let home = self.codex_home_for(seat);
        let Some(key) = dispatch_state_trust_key(&home.join("hooks.json")) else {
            return false;
        };
        std::fs::read_to_string(home.join("config.toml"))
            .is_ok_and(|config| codex_hooks_trusted(&config, &key))
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
        let probe = LiveHookProbe {
            state_dir: self.state_dir.clone(),
        };
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
                            probe: &probe,
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
    /// A probe with fixed answers: the pane's directory, and whether the hook
    /// state has appeared since the scan's snapshot.
    struct FakeProbe {
        cwd: Option<String>,
        present: bool,
    }

    impl HookProbe for FakeProbe {
        fn pane_cwd(&self, _socket: &str, _pane: &str) -> Option<String> {
            self.cwd.clone()
        }
        fn state_present(&self, _names: &[String]) -> bool {
            self.present
        }
    }

    struct Fixture {
        _tree: tempfile::TempDir,
        worktree: String,
        full: Value,
        rollouts: BTreeMap<String, Vec<Rollout>>,
        newest: BTreeMap<(String, String, String), (DateTime<Utc>, String)>,
        since: DateTime<Utc>,
    }

    /// One dispatched codex pane: an open issue bound to session `s1`, whose
    /// rollout in the worktree produced a first assistant turn.
    fn hook_fixture(rollouts: &[(&str, &str, Option<&str>, Option<&str>)]) -> Fixture {
        let tree = tempfile::tempdir().unwrap();
        let worktree = tree.path().join("worktree");
        let day = tree.path().join(".codex/sessions/2026/09/13");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&day).unwrap();
        for (session, started, assistant, originator) in rollouts {
            let mut meta = json!({"session_id": session, "cwd": worktree, "timestamp": started});
            if let Some(originator) = originator {
                meta["originator"] = json!(originator);
            }
            let mut body = vec![
                json!({"timestamp": started, "type": "session_meta", "payload": meta}).to_string(),
            ];
            if let Some(at) = assistant {
                body.push(
                    json!({"timestamp": at, "type": "response_item", "payload": {
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "on it"}]}})
                    .to_string(),
                );
            }
            std::fs::write(
                day.join(format!("rollout-2026-09-13T00-00-30-{session}.jsonl")),
                body.join("\n"),
            )
            .unwrap();
        }
        let full = json!({"issue": {"uid": "issue-uid", "status": "open", "metadata": {
            "dispatch": {"id": "dispatch-1", "harness": "codex", "seat": "codex-01",
                "host": "macazbd", "pane": "%542", "tmux_socket": "default",
                "worktree": worktree, "dispatched_at": "2026-09-13T00:00:00Z"},
            "kickoff": {"id": "dispatch-1", "state": "accepted",
                "session_observed": "s1", "at": "2026-09-13T00:00:20Z"}
        }}});
        let since = timestamp(&json!("2026-09-12T00:00:00Z")).unwrap();
        Fixture {
            worktree: worktree.to_string_lossy().into_owned(),
            rollouts: rollout_index(&[tree.path().join(".codex/sessions")], since),
            newest: newest_pane_dispatches(&json!({"issues": [full["issue"].clone()]})),
            full,
            since,
            _tree: tree,
        }
    }

    #[test]
    fn hook_state_names_follow_the_socket_qualified_contract() {
        assert_eq!(pane_number("%542").as_deref(), Some("542"));
        for not_a_pane in ["%", "-", "%-", "%5a2", ""] {
            assert!(pane_number(not_a_pane).is_none(), "{not_a_pane}");
        }
        assert_eq!(
            hook_state_names("jilog#4nd2", "default", "542"),
            vec!["jilog-4nd2-default-542.json", "jilog-4nd2-542.json"]
        );
        assert_eq!(
            hook_state_names("jilog#4nd2", "kwt", "542"),
            vec!["jilog-4nd2-kwt-542.json"]
        );
    }

    #[test]
    fn hook_trust_is_read_for_the_dispatch_state_hook_alone() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks.json");
        std::fs::write(
            &hooks,
            json!({"hooks": {"SessionStart": [
                {"hooks": [{"type": "command",
                    "command": "/opt/homebrew/bin/python3 /runner/hooks.py startup"}]},
                {"hooks": [
                    {"type": "other", "command": "noise"},
                    {"type": "command",
                     "command": "/opt/homebrew/bin/python3 /runner/hooks.py dispatch-state"}]}
            ]}})
            .to_string(),
        )
        .unwrap();
        let key = dispatch_state_trust_key(&hooks).expect("the dispatch-state entry");
        assert!(key.ends_with(":session_start:1:1"), "{key}");
        assert!(key.starts_with(&std::fs::canonicalize(&hooks).unwrap().display().to_string()));

        let trusted = |k: &str| {
            codex_hooks_trusted(
                &format!("[hooks.state.\"{k}\"]\ntrusted_hash = \"sha256:a\"\n"),
                &key,
            )
        };
        assert!(trusted(&key));
        // The sibling startup hook's trust is not this hook's trust, and
        // neither is a hash for another event.
        assert!(!trusted(&key.replace(":1:1", ":0:0")));
        assert!(!trusted(&key.replace("session_start", "pre_tool_use")));
        // An empty hash, a missing table, and unparseable TOML are untrusted.
        assert!(!codex_hooks_trusted(
            &format!("[hooks.state.\"{key}\"]\ntrusted_hash = \"\"\n"),
            &key
        ));
        assert!(!codex_hooks_trusted("trusted_hash = \"sha256:a\"\n", &key));
        assert!(!codex_hooks_trusted("[hooks.state\n", &key));

        // An impostor that only ends in the right word is not the bridge.
        std::fs::write(
            &hooks,
            json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command",
                "command": "/bin/echo /runner/hooks.py dispatch-state"}]}]}})
            .to_string(),
        )
        .unwrap();
        assert!(dispatch_state_trust_key(&hooks).is_none());
        assert!(dispatch_state_trust_key(&dir.path().join("absent.json")).is_none());
    }

    #[test]
    fn missing_hook_state_reports_a_confirmed_live_pane() {
        let f = hook_fixture(&[(
            "s1",
            "2026-09-13T00:00:30Z",
            Some("2026-09-13T00:01:00Z"),
            None,
        )]);
        let empty = BTreeSet::new();
        let probe = FakeProbe {
            cwd: Some(f.worktree.clone()),
            present: false,
        };
        let evidence = HookEvidence {
            host: "macazbd",
            state_files: &empty,
            newest_pane: &f.newest,
            rollouts: &f.rollouts,
            hooks_trusted: true,
            probe: &probe,
        };
        let (handle, message) =
            missing_hook_state_record(&f.full, "jilog#4nd2", f.since, &evidence)
                .expect("a live pane with a first turn and no hook state");
        assert_eq!(handle.session_id, "dispatch-1:codex_missing_hook_state");
        assert_eq!(message.name.as_deref(), Some("codex_missing_hook_state"));
        let content = message.content.as_ref().unwrap();
        assert_eq!(content["seat"].as_str(), Some("codex-01"));
        let detail = content["error"].as_str().unwrap();
        for expected in ["jilog-4nd2-default-542.json", "%542", "session s1"] {
            assert!(
                detail.contains(expected),
                "{expected} missing from {detail}"
            );
        }
        assert_eq!(
            handle.modified,
            timestamp(&json!("2026-09-13T00:01:00Z")).unwrap()
        );
    }

    #[test]
    fn missing_hook_state_binds_the_turn_to_the_dispatched_session() {
        let live = |f: &Fixture| FakeProbe {
            cwd: Some(f.worktree.clone()),
            present: false,
        };
        let empty = BTreeSet::new();
        let check = |f: &Fixture, probe: &FakeProbe| {
            missing_hook_state_record(
                &f.full,
                "jilog#4nd2",
                f.since,
                &HookEvidence {
                    host: "macazbd",
                    state_files: &empty,
                    newest_pane: &f.newest,
                    rollouts: &f.rollouts,
                    hooks_trusted: true,
                    probe,
                },
            )
        };

        // Another session in the same worktree — a rescue, a `codex resume` —
        // is not this pane's evidence, even when it produced the only turn.
        let other = hook_fixture(&[
            ("s1", "2026-09-13T00:00:30Z", None, None),
            (
                "s9",
                "2026-09-13T00:02:00Z",
                Some("2026-09-13T00:03:00Z"),
                None,
            ),
        ]);
        assert!(check(&other, &live(&other)).is_none());

        // The bound session's own turn is what counts, whichever rollout is
        // newest or quietest.
        let bound = hook_fixture(&[
            (
                "s1",
                "2026-09-13T00:00:30Z",
                Some("2026-09-13T00:01:00Z"),
                None,
            ),
            (
                "s9",
                "2026-09-13T00:02:00Z",
                Some("2026-09-13T00:03:00Z"),
                None,
            ),
        ]);
        let record = check(&bound, &live(&bound)).expect("the bound session produced a turn");
        assert_eq!(
            record.0.modified,
            timestamp(&json!("2026-09-13T00:01:00Z")).unwrap()
        );

        // With no bound session, a non-interactive `codex exec` run in the
        // worktree never carries dispatch markers and proves nothing.
        let mut exec = hook_fixture(&[(
            "s7",
            "2026-09-13T00:00:30Z",
            Some("2026-09-13T00:01:00Z"),
            Some("codex_exec"),
        )]);
        exec.full["issue"]["metadata"]["kickoff"]["session_observed"] = json!("-");
        assert!(check(&exec, &live(&exec)).is_none());

        // A kickoff record from an earlier dispatch does not bind this one;
        // an interactive rollout then still counts.
        let mut stale = hook_fixture(&[(
            "s7",
            "2026-09-13T00:00:30Z",
            Some("2026-09-13T00:01:00Z"),
            None,
        )]);
        stale.full["issue"]["metadata"]["kickoff"]["id"] = json!("dispatch-0");
        assert!(check(&stale, &live(&stale)).is_some());

        // A rollout that predates the dispatch belongs to earlier work.
        let mut early = hook_fixture(&[(
            "s1",
            "2026-09-12T23:00:00Z",
            Some("2026-09-12T23:01:00Z"),
            None,
        )]);
        early.full["issue"]["metadata"]["dispatch"]["dispatched_at"] =
            json!("2026-09-13T00:00:00Z");
        assert!(check(&early, &live(&early)).is_none());

        // A session that never answered proves nothing about the hook.
        let quiet = hook_fixture(&[("s1", "2026-09-13T00:00:30Z", None, None)]);
        assert!(check(&quiet, &live(&quiet)).is_none());
    }

    #[test]
    fn missing_hook_state_is_silent_without_a_confirmed_live_identity() {
        let f = hook_fixture(&[(
            "s1",
            "2026-09-13T00:00:30Z",
            Some("2026-09-13T00:01:00Z"),
            None,
        )]);
        let live = FakeProbe {
            cwd: Some(f.worktree.clone()),
            present: false,
        };
        let empty = BTreeSet::new();
        let base = HookEvidence {
            host: "macazbd",
            state_files: &empty,
            newest_pane: &f.newest,
            rollouts: &f.rollouts,
            hooks_trusted: true,
            probe: &live,
        };
        let check = |issue: &Value, evidence: &HookEvidence| {
            missing_hook_state_record(issue, "jilog#4nd2", f.since, evidence)
        };
        assert!(check(&f.full, &base).is_some());

        // Valid hooks: either the socket-qualified file or its default-server
        // transition copy is proof the hook ran.
        for name in ["jilog-4nd2-default-542.json", "jilog-4nd2-542.json"] {
            let present = BTreeSet::from([name.to_string()]);
            assert!(
                check(
                    &f.full,
                    &HookEvidence {
                        state_files: &present,
                        ..base
                    }
                )
                .is_none(),
                "{name} must suppress the kind"
            );
        }

        // A hook state file written after this scan's snapshot — the pane was
        // still starting — disproves the finding before it is filed.
        let appeared = FakeProbe {
            cwd: Some(f.worktree.clone()),
            present: true,
        };
        assert!(check(
            &f.full,
            &HookEvidence {
                probe: &appeared,
                ..base
            }
        )
        .is_none());

        // Teardown: a terminal marker, or a worktree that is gone.
        let ended = BTreeSet::from(["jilog-4nd2-default-542.ended.s1".to_string()]);
        assert!(check(
            &f.full,
            &HookEvidence {
                state_files: &ended,
                ..base
            }
        )
        .is_none());
        let mut removed = f.full.clone();
        removed["issue"]["metadata"]["dispatch"]["worktree"] =
            json!(format!("{}/gone", f.worktree));
        assert!(check(&removed, &base).is_none());

        // A closed issue is a finished dispatch: the liveness watcher deletes
        // the hook state file and its markers on a clean disarm.
        let mut closed = f.full.clone();
        closed["issue"]["status"] = json!("closed");
        assert!(check(&closed, &base).is_none());

        // A pane that is gone, or one that has moved on to other work, leaves
        // no live identity to confirm.
        for cwd in [None, Some(format!("{}/..", f.worktree))] {
            let gone = FakeProbe {
                cwd,
                present: false,
            };
            assert!(check(
                &f.full,
                &HookEvidence {
                    probe: &gone,
                    ..base
                }
            )
            .is_none());
        }

        // Remote dispatch: another host's state directory is not readable here.
        assert!(check(
            &f.full,
            &HookEvidence {
                host: "joimba",
                ..base
            }
        )
        .is_none());

        // Pane reuse: a later dispatch owns pane %542 now.
        let mut later = f.full["issue"].clone();
        later["metadata"]["dispatch"]["id"] = json!("dispatch-2");
        later["metadata"]["dispatch"]["dispatched_at"] = json!("2026-09-13T01:00:00Z");
        let reused = newest_pane_dispatches(&json!({"issues": [f.full["issue"].clone(), later]}));
        assert_eq!(reused.len(), 1);
        assert!(check(
            &f.full,
            &HookEvidence {
                newest_pane: &reused,
                ..base
            }
        )
        .is_none());

        // A phase-one record with no pane yet is not a pane identity.
        let mut placeholder = f.full.clone();
        placeholder["issue"]["metadata"]["dispatch"]["pane"] = json!("-");
        assert!(check(&placeholder, &base).is_none());

        // Untrusted hooks: the pane stops at the hook-review dialog instead.
        assert!(check(
            &f.full,
            &HookEvidence {
                hooks_trusted: false,
                ..base
            }
        )
        .is_none());

        // Claude panes install hooks by a different path; out of scope here.
        let mut claude = f.full.clone();
        claude["issue"]["metadata"]["dispatch"]["harness"] = json!("claude");
        assert!(check(&claude, &base).is_none());

        // Evidence older than the scan window is not refiled.
        let later_since = timestamp(&json!("2026-09-13T02:00:00Z")).unwrap();
        assert!(missing_hook_state_record(&f.full, "jilog#4nd2", later_since, &base).is_none());
    }

    #[test]
    fn a_dispatch_without_a_pool_seat_reads_the_main_codex_home() {
        let reader = WorkerSignalsReader {
            pool_dir: PathBuf::from("/pool"),
            codex_home: PathBuf::from("/main"),
            ..WorkerSignalsReader::default()
        };
        for seat in [None, Some("main"), Some("-")] {
            assert_eq!(
                reader.codex_home_for(seat),
                PathBuf::from("/main"),
                "{seat:?}"
            );
        }
        assert_eq!(
            reader.codex_home_for(Some("codex-01")),
            PathBuf::from("/pool/profiles/codex-01")
        );
    }
}
