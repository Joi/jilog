//! KataTracker — shells out to the `kata` CLI.
//!
//! [kata](https://github.com/wesm/kata) is a local-first issue tracker with a
//! user-global SQLite store behind a daemon. Unlike beads (per-repo `.beads/`
//! directories), kata has one DB containing many named projects.
//!
//! ## Config
//!
//! ```toml
//! [tracker]
//! type = "kata"
//! project = "jilog"
//! ```
//!
//! `project` is the kata project name (set via `kata init --project <name>` in
//! the project's workspace directory).
//!
//! ## Environment
//!
//! Set `KATA_AUTHOR` to attribute writes (e.g. `KATA_AUTHOR=jilog-extractor`).
//! Defaults: `$KATA_AUTHOR` > `$USER` > git config name > `anonymous`.
//!
//! ## CLI mapping
//!
//! | Trait method     | CLI invocation |
//! |------------------|----------------|
//! | `create()`       | `kata --project <p> --json create "<title>" --body "..." --label jilog --label jilog:<kind> --idempotency-key <key>` |
//! | `list_open()`    | `kata --project <p> --json list --status open` |
//! | `is_resolved()`  | `kata --project <p> --json show <number>` → status == "closed" |
//!
//! ## Label charset
//!
//! kata enforces `[a-z0-9._:-]` length 1..64 on labels. We use `:` (not `/`)
//! as the kind separator: `jilog`, `jilog:correction`, `jilog:error`, etc.

use std::process::Command;

use serde::Deserialize;

use crate::error::JilogReviewError;
use crate::signal::Signal;
use crate::tracker::{IssueRef, Tracker, signal_title};

/// Tracker backed by the `kata` CLI against a named kata project.
pub struct KataTracker {
    pub project: String,
}

impl KataTracker {
    pub fn new(project: impl Into<String>) -> Self {
        Self { project: project.into() }
    }

    /// Build a `kata` command pre-configured with `--project <name> --json`.
    fn cmd(&self) -> Command {
        let mut c = Command::new("kata");
        c.args(["--project", &self.project, "--json"]);
        c
    }
}

// ---------------------------------------------------------------------------
// JSON schemas for parsing kata output
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct KataIssue {
    number: u64,
    title: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct KataList {
    #[serde(default)]
    issues: Vec<KataIssue>,
}

#[derive(Debug, Deserialize)]
struct KataShow {
    issue: KataIssue,
}

#[derive(Debug, Deserialize)]
struct KataCreate {
    issue: KataIssue,
}

/// kata returns structured errors as `{"error":{"kind":..,"code":..,"message":..}}`.
#[derive(Debug, Deserialize)]
struct KataErrorWrapper {
    error: KataErrorBody,
}

#[derive(Debug, Deserialize)]
struct KataErrorBody {
    kind: String,
    message: String,
}

// ---------------------------------------------------------------------------
// Tracker impl
// ---------------------------------------------------------------------------

impl Tracker for KataTracker {
    fn name(&self) -> &str {
        "kata"
    }

    fn create(&self, signal: &Signal) -> Result<IssueRef, JilogReviewError> {
        let title = signal_title(signal);

        // Trait contract: title-match against open issues for dedup.
        let open = self.list_open()?;
        if let Some(existing) = open.iter().find(|i| i.title == title) {
            return Ok(existing.clone());
        }

        let body = format!(
            "Detected by jilog review pipeline.\n\nSession: {}\nKind: {}",
            signal.session_id(),
            signal.kind()
        );

        // kata also enforces idempotency at the daemon level: if the same key
        // arrives twice, kata returns a `duplicate_candidates` error. The
        // `signal_title` is already deterministic, so we use a slugified
        // version of it as the key — second-layer safety beyond list_open().
        let idem = idempotency_key(&title);

        // kata label charset is [a-z0-9._:-], so we use `:` as the kind sep.
        let kind_label = format!("jilog:{}", signal.kind());

        let output = self
            .cmd()
            .args([
                "create",
                &title,
                "--body",
                &body,
                "--label",
                "jilog",
                "--label",
                &kind_label,
                "--idempotency-key",
                &idem,
            ])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("kata create failed: {}", e)))?;

        if !output.status.success() {
            return Err(parse_kata_error(&output.stdout, &output.stderr, "create"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: KataCreate = serde_json::from_str(&stdout).map_err(|e| {
            JilogReviewError::Tracker(format!(
                "kata create JSON parse: {} (stdout: {})",
                e,
                stdout.chars().take(200).collect::<String>()
            ))
        })?;

        Ok(IssueRef {
            id: format!("#{}", parsed.issue.number),
            backend: "kata".to_string(),
            url: None,
            title,
        })
    }

    fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
        let output = self
            .cmd()
            .args(["list", "--status", "open"])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("kata list failed: {}", e)))?;

        if !output.status.success() {
            return Err(parse_kata_error(&output.stdout, &output.stderr, "list"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: KataList = serde_json::from_str(&stdout).unwrap_or(KataList { issues: vec![] });

        Ok(parsed
            .issues
            .into_iter()
            .filter(|i| i.status == "open")
            .map(|i| IssueRef {
                id: format!("#{}", i.number),
                backend: "kata".to_string(),
                url: None,
                title: i.title,
            })
            .collect())
    }

    fn is_resolved(&self, issue: &IssueRef) -> Result<bool, JilogReviewError> {
        // Strip leading '#' for the CLI; accept either form defensively.
        let n = issue.id.trim_start_matches('#');

        let output = self
            .cmd()
            .args(["show", n])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("kata show failed: {}", e)))?;

        if !output.status.success() {
            // Treat unknown / lookup failure as unresolved so callers can
            // re-create the issue rather than silently dropping the signal.
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: KataShow = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        Ok(parsed.issue.status == "closed")
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a deterministic idempotency key from a signal title.
///
/// kata accepts arbitrary strings for `--idempotency-key`. We slugify the
/// signal title (whitespace → `-`) and clamp length to 240 chars. The
/// `signal_title` function is itself deterministic, so re-running the
/// extractor on the same digest produces the same key.
fn idempotency_key(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    if slug.chars().count() > 240 {
        slug.chars().take(240).collect()
    } else {
        slug
    }
}

/// Best-effort parse of kata's structured JSON error output. Falls back to
/// stderr if the JSON shape doesn't match.
fn parse_kata_error(stdout: &[u8], stderr: &[u8], op: &str) -> JilogReviewError {
    // kata emits errors on stderr as JSON like:
    // {"error":{"kind":"validation","code":"validation","message":"...","exit_code":3}}
    let stderr_str = String::from_utf8_lossy(stderr);
    let stdout_str = String::from_utf8_lossy(stdout);

    for blob in [&stderr_str, &stdout_str] {
        for line in blob.lines() {
            if let Ok(wrapped) = serde_json::from_str::<KataErrorWrapper>(line) {
                return JilogReviewError::Tracker(format!(
                    "kata {} {}: {}",
                    op, wrapped.error.kind, wrapped.error.message
                ));
            }
        }
    }

    JilogReviewError::Tracker(format!("kata {} failed: {}", op, stderr_str.trim()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kata_tracker_graceful_when_kata_missing_or_unconfigured() {
        // If kata is not on PATH, we get a Command error.
        // If kata is on PATH but the project doesn't exist, we get a Tracker error.
        // Either way, no panic.
        let tracker = KataTracker::new("nonexistent-jilog-test-project");
        let signal = Signal::Correction(crate::signal::Correction {
            session_id: "test".into(),
            context: "some correction context here".into(),
        });

        match tracker.list_open() {
            Ok(_) => eprintln!("kata list returned successfully (project may exist)"),
            Err(JilogReviewError::Command(msg)) => {
                eprintln!("kata not found (expected in CI): {}", msg);
            }
            Err(JilogReviewError::Tracker(msg)) => {
                eprintln!("kata returned error (expected when project missing): {}", msg);
            }
            Err(e) => {
                eprintln!("unexpected error type: {}", e);
            }
        }
        let _ = signal;
    }

    #[test]
    fn idempotency_key_is_deterministic() {
        let title = "[jilog/correction] sess-abc: foo bar baz";
        let k1 = idempotency_key(title);
        let k2 = idempotency_key(title);
        assert_eq!(k1, k2, "same input must produce same key");
    }

    #[test]
    fn idempotency_key_strips_whitespace() {
        let key = idempotency_key("[jilog/correction] sess-abc: foo bar");
        assert!(!key.contains(' '), "key must not contain spaces: {}", key);
        assert!(key.contains('-'), "spaces should be replaced with -");
    }

    #[test]
    fn idempotency_key_clamps_length() {
        let long = "x".repeat(1000);
        let key = idempotency_key(&long);
        assert!(key.chars().count() <= 240, "key too long: {} chars", key.chars().count());
    }

    #[test]
    fn idempotency_key_handles_unicode() {
        // Title may contain Japanese, em-dashes, etc. — must not panic on byte vs char boundaries.
        let title = "[jilog/error] 失敗: 茶の湯 — em-dash and 日本語";
        let key = idempotency_key(title);
        assert!(!key.is_empty());
        assert!(key.chars().count() <= 240);
    }

    #[test]
    fn parse_kata_error_extracts_structured_message() {
        let stderr = br#"{"error":{"kind":"validation","code":"validation","message":"label must match charset [a-z0-9._:-] and length 1..64","exit_code":3}}"#;
        let err = parse_kata_error(b"", stderr, "create");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("validation"), "missing kind in: {}", msg);
                assert!(msg.contains("charset"), "missing message in: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }

    #[test]
    fn parse_kata_error_falls_back_to_stderr_text() {
        let err = parse_kata_error(b"", b"raw error not json", "list");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("raw error not json"), "expected stderr in: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }
}
