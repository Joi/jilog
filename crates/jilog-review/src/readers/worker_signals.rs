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
use std::{path::PathBuf, process::Command, sync::Mutex};

pub struct WorkerSignalsReader {
    pub pool_dir: PathBuf,
    pub kata_bin: PathBuf,
    evidence: Mutex<Vec<(TranscriptHandle, Message)>>,
}

impl Default for WorkerSignalsReader {
    fn default() -> Self {
        Self {
            pool_dir: expand_tilde("~/.codex-pool"),
            kata_bin: "kata".into(),
            evidence: Mutex::new(Vec::new()),
        }
    }
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
                Ok(full) => records.extend(dispatch_records(&full, since)),
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
}
