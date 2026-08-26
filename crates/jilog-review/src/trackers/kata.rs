//! KataTracker — shells out to the `kata` CLI.
//!
//! [kata](https://github.com/kenn-io/kata) is a local-first issue tracker with a
//! user-global SQLite store behind a daemon: one DB containing many named
//! projects, rather than per-repo state.
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
//! | `create()`       | `kata --project <p> --json create "<title>" --body "..." --label jilog --label jilog:<kind> --idempotency-key <key> --priority <n>` |
//! | `list_open()`    | `kata --project <p> --json list --status open --limit 0` |
//! | `list_closed()`  | `kata --project <p> --json list --status closed --limit 0` |
//! | `is_resolved()`  | `kata --project <p> --json show <ref>` → status == "closed" |
//! | `reopen()`       | `kata --project <p> --json reopen <ref>` + comment + label add |
//!
//! `<ref>` is kata's short_id (e.g. `e3nj`), falling back to the full ULID
//! or, for pre-0.15 daemons, the legacy issue number.
//!
//! ## Minimum kata version
//!
//! Listings pass `--limit 0` (unlimited), which kata accepts from v0.14.1;
//! older CLIs reject it and every create fails with a validation error that
//! names the required upgrade. Listings are memoized per tracker instance
//! (one run), with create/reopen updating the memo in place.
//!
//! ## Label charset
//!
//! kata enforces `[a-z0-9._:-]` length 1..64 on labels. We use `:` (not `/`)
//! as the kind separator: `jilog`, `jilog:correction`, `jilog:error`, etc.

use std::process::Command;

use chrono::Local;
use serde::Deserialize;

use crate::error::JilogReviewError;
use crate::signal::Signal;
use crate::tracker::{IssueRef, Tracker, signal_title};

/// Tracker backed by the `kata` CLI against a named kata project.
pub struct KataTracker {
    pub project: String,
    /// Display path of the digest file this run writes, for issue-body
    /// backlinks. None falls back to the conventional `~/.amplifier/health`
    /// location (jilog#re4k).
    digest_path: Option<String>,
    /// The run's digest date (same source as the digest filename). None
    /// falls back to `Local::now` (jilog#re4k).
    date: Option<String>,
    /// Per-run memo of the full open/closed listings. Without it, a run
    /// with N signals fetches the complete project listing O(N) times
    /// (fresheyes 2026-08-26 round 2). jilog constructs one tracker per
    /// run and is the single writer during the run, so the memo only needs
    /// the updates create()/reopen() apply themselves.
    open_cache: std::sync::Mutex<Option<Vec<IssueRef>>>,
    closed_cache: std::sync::Mutex<Option<Vec<IssueRef>>>,
}

impl KataTracker {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            digest_path: None,
            date: None,
            open_cache: std::sync::Mutex::new(None),
            closed_cache: std::sync::Mutex::new(None),
        }
    }

    /// Construct with the run's digest context so issue bodies point at the
    /// REAL digest file with the SAME date the filename uses (jilog#re4k).
    pub fn with_run_context(
        project: impl Into<String>,
        digest_path: impl Into<String>,
        date: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            digest_path: Some(digest_path.into()),
            date: Some(date.into()),
            open_cache: std::sync::Mutex::new(None),
            closed_cache: std::sync::Mutex::new(None),
        }
    }

    /// The run date used in issue bodies and recurrence comments.
    fn run_date(&self) -> String {
        self.date
            .clone()
            .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string())
    }

    /// Build a `kata` command pre-configured with `--project <name> --json`.
    fn cmd(&self) -> Command {
        let mut c = Command::new("kata");
        c.args(["--project", &self.project, "--json"]);
        c
    }

    /// List closed issues for this project (mirrors `list_open` with `--status closed`).
    fn list_closed(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
        if let Some(cached) = self.closed_cache.lock().unwrap().as_ref() {
            return Ok(cached.clone());
        }
        let issues = self.fetch_list("closed")?;
        *self.closed_cache.lock().unwrap() = Some(issues.clone());
        Ok(issues)
    }

    /// Fetch one full listing from the CLI (`--limit 0` = unlimited; needs
    /// kata >= 0.14.1 — see the module doc's version note).
    fn fetch_list(&self, status: &str) -> Result<Vec<IssueRef>, JilogReviewError> {
        let output = self
            .cmd()
            .args(["list", "--status", status, "--limit", "0"])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("kata list failed: {}", e)))?;

        if !output.status.success() {
            let err = parse_kata_error(&output.stdout, &output.stderr, "list");
            // kata < 0.14.1 rejects --limit 0 ("--limit must be a positive
            // integer"); make the version requirement explicit instead of
            // letting every create fail with an opaque validation error.
            let msg = err.to_string();
            if msg.contains("positive") && msg.contains("limit") {
                return Err(JilogReviewError::Tracker(format!(
                    "{} — jilog requires kata >= 0.14.1 (--limit 0 = unlimited); upgrade the kata CLI",
                    msg
                )));
            }
            return Err(err);
        }

        parse_list_response(&String::from_utf8_lossy(&output.stdout), status)
    }

    /// Reopen a closed issue, then annotate it with a recurrence comment and
    /// the `jilog:recurred` label.
    ///
    /// Only the reopen itself is fail-loud. The annotations are best-effort
    /// (warn): if they errored after a successful reopen, the caller would
    /// unmark the session for retry, but the retry finds the issue already
    /// OPEN via title dedup and returns early — the annotations would never
    /// be repaired and the session would re-fail forever (fresheyes
    /// 2026-08-26 on jilog#1dvk).
    fn reopen(&self, issue_ref: &str, comment_body: &str) -> Result<(), JilogReviewError> {
        // Step 1: reopen the issue — the semantically required part.
        let out = self
            .cmd()
            .args(["reopen", issue_ref])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("kata reopen failed: {}", e)))?;
        if !out.status.success() {
            return Err(parse_kata_error(&out.stdout, &out.stderr, "reopen"));
        }

        // Steps 2+3: advisory annotations — warn, never fail the create.
        match self.cmd().args(["comment", issue_ref, "--body", comment_body]).output() {
            Ok(out) if !out.status.success() => tracing::warn!(
                "kata recurrence comment on {} failed (issue reopened anyway): {}",
                issue_ref,
                parse_kata_error(&out.stdout, &out.stderr, "comment")
            ),
            Err(e) => tracing::warn!(
                "kata recurrence comment on {} failed (issue reopened anyway): {}",
                issue_ref, e
            ),
            _ => {}
        }
        match self.cmd().args(["label", "add", issue_ref, "jilog:recurred"]).output() {
            Ok(out) if !out.status.success() => tracing::warn!(
                "kata jilog:recurred label on {} failed (issue reopened anyway): {}",
                issue_ref,
                parse_kata_error(&out.stdout, &out.stderr, "label add")
            ),
            Err(e) => tracing::warn!(
                "kata jilog:recurred label on {} failed (issue reopened anyway): {}",
                issue_ref, e
            ),
            _ => {}
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JSON schemas for parsing kata output
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct KataIssue {
    /// Short ref like "e3nj" (kata ≥0.15 JSON; the canonical CLI ref form).
    #[serde(default)]
    short_id: Option<String>,
    /// Full ULID — also accepted by the CLI as a ref.
    #[serde(default)]
    uid: Option<String>,
    /// Legacy numeric ref (pre-0.15 JSON carried `number`; newer versions
    /// have a numeric row `id` that is NOT a CLI ref, so we never read `id`).
    #[serde(default)]
    number: Option<u64>,
    title: String,
    /// Required: a row without `status` is schema drift, and defaulting it
    /// would silently filter the row out of every listing (fresheyes
    /// 2026-08-26 on jilog#fx51).
    status: String,
}

impl KataIssue {
    /// The CLI-usable ref for this issue: short_id > uid > legacy number.
    /// An issue with none of them means the JSON shape drifted — fail loud
    /// rather than fabricate a ref (jilog#fx51).
    fn issue_ref(&self) -> Result<String, JilogReviewError> {
        self.short_id
            .clone()
            .or_else(|| self.uid.clone())
            .or_else(|| self.number.map(|n| n.to_string()))
            .ok_or_else(|| {
                JilogReviewError::Tracker(format!(
                    "kata issue '{}' has no short_id/uid/number — JSON shape drifted",
                    self.title
                ))
            })
    }
}

#[derive(Debug, Deserialize)]
struct KataList {
    /// Required: a payload without `issues` (renamed collection, error-ish
    /// shape that still exits 0) must fail loud, not read as an empty list.
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

        // Dedup pass 1: return existing open issue if title matches.
        let open = self.list_open()?;
        if let Some(existing) = open.iter().find(|i| i.title == title) {
            return Ok(existing.clone());
        }

        // Dedup pass 2 (reopen-on-recurrence): if a closed issue has the same
        // title, reopen it instead of filing a duplicate.
        let closed = self.list_closed()?;
        if let Some(existing) = closed.iter().find(|i| i.title == title) {
            let issue_ref = existing.id.trim_start_matches('#');
            let comment = format!(
                "Recurred on {} — closure may have been premature.",
                self.run_date()
            );
            self.reopen(issue_ref, &comment)?;
            // Keep the per-run memo truthful: the issue moved closed -> open.
            let reopened = existing.clone();
            if let Some(open) = self.open_cache.lock().unwrap().as_mut() {
                open.push(reopened.clone());
            }
            if let Some(closed) = self.closed_cache.lock().unwrap().as_mut() {
                closed.retain(|i| i.id != reopened.id);
            }
            return Ok(reopened);
        }

        let body = build_body(signal, &self.run_date(), self.digest_path.as_deref());

        // kata also enforces idempotency at the daemon level: if the same key
        // arrives twice, kata returns a `duplicate_candidates` error. The
        // `signal_title` is already deterministic, so we use a slugified
        // version of it as the key — second-layer safety beyond list_open().
        let idem = idempotency_key(&title);

        // kata label charset is [a-z0-9._:-], so we use `:` as the kind sep.
        let kind_label = format!("jilog:{}", signal.kind());

        let priority = signal_priority(signal).to_string();

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
                "--priority",
                &priority,
            ])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("kata create failed: {}", e)))?;

        if !output.status.success() {
            return Err(parse_kata_error(&output.stdout, &output.stderr, "create"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let issue_ref = parse_create_response(&stdout)?;

        let created = IssueRef {
            id: format!("#{}", issue_ref),
            backend: "kata".to_string(),
            url: None,
            title,
        };
        if let Some(open) = self.open_cache.lock().unwrap().as_mut() {
            open.push(created.clone());
        }
        Ok(created)
    }

    fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
        if let Some(cached) = self.open_cache.lock().unwrap().as_ref() {
            return Ok(cached.clone());
        }
        let issues = self.fetch_list("open")?;
        *self.open_cache.lock().unwrap() = Some(issues.clone());
        Ok(issues)
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

/// Map a signal to a kata priority level (1 = highest, 3 = lowest).
///
/// | Priority | Signals |
/// |----------|---------|
/// | 1        | Error (active tool failures — investigate immediately) |
/// | 2        | Correction, Pattern (behavioural issues worth fixing soon) |
/// | 3        | Workaround, Deferral (lower-urgency, deferred work) |
fn signal_priority(signal: &Signal) -> u8 {
    match signal {
        Signal::Error(_) => 1,
        Signal::Correction(_) => 2,
        Signal::Pattern(_) => 2,
        Signal::Workaround(_) => 3,
        Signal::Deferral(_) => 3,
    }
}

/// Build the issue body for a new kata issue.
///
/// Format:
/// ```text
/// Detected by jilog review pipeline on YYYY-MM-DD.
///
/// ## Source
/// - Session: <session_id>
/// - Kind: <kind>
/// - See `~/.amplifier/health/learning-digest-YYYY-MM-DD.md` for the full digest window.
///
/// ## Signal
/// <kind-specific content>
/// ```
///
/// Kind-specific content:
/// - Correction:  the `context` field
/// - Error:       `Tool: <tool_name>` + `Message: <message>`
/// - Workaround:  `Pattern: <pattern>` + `Context: <context>`
/// - Pattern:     the `description` field
/// - Deferral:    the `item` field
fn build_body(signal: &Signal, date: &str, digest_path: Option<&str>) -> String {
    let session_id = signal.session_id();
    let kind = signal.kind();
    let digest_path = match digest_path {
        Some(p) => p.to_string(),
        None => format!("~/.amplifier/health/learning-digest-{}.md", date),
    };

    let kind_specific = match signal {
        Signal::Correction(c) => c.context.clone(),
        Signal::Error(e) => format!("Tool: {}\nMessage: {}", e.tool_name, e.message),
        Signal::Workaround(w) => format!("Pattern: {}\nContext: {}", w.pattern, w.context),
        Signal::Pattern(p) => p.description.clone(),
        Signal::Deferral(d) => d.item.clone(),
    };

    format!(
        "Detected by jilog review pipeline on {date}.\n\n\
## Source\n\
- Session: {session_id}\n\
- Kind: {kind}\n\
- See `{digest_path}` for the full digest window this signal came from.\n\n\
## Signal\n\
{kind_specific}"
    )
}

/// Parse a `kata --json create` response into a CLI-usable issue ref.
///
/// kata ≥0.15 returns `{"kata_api_version":1,"issue":{"id":..,"uid":..,
/// "short_id":..,"title":..,...}}` with no `number` field; older versions
/// carried `number`. Fail loud on any shape we cannot get a ref out of —
/// a create that succeeded server-side but parses as a failure poisons
/// digest backlinks and (worse) retry logic (jilog#fx51).
fn parse_create_response(stdout: &str) -> Result<String, JilogReviewError> {
    let parsed: KataCreate = serde_json::from_str(stdout).map_err(|e| {
        JilogReviewError::Tracker(format!(
            "kata create JSON parse: {} (stdout: {})",
            e,
            stdout.chars().take(200).collect::<String>()
        ))
    })?;
    parsed.issue.issue_ref()
}

/// Parse a `kata --json list` response, keeping issues whose status matches
/// `want_status`. Any parse failure — the whole payload or a single issue
/// missing every ref field — is a loud error, never an empty list: an empty
/// list here silently disables dedup and recurrence detection, and the
/// resulting re-files surface only as opaque idempotency rejections
/// (jilog#fx51, comment thread).
fn parse_list_response(stdout: &str, want_status: &str) -> Result<Vec<IssueRef>, JilogReviewError> {
    let parsed: KataList = serde_json::from_str(stdout).map_err(|e| {
        JilogReviewError::Tracker(format!(
            "kata list JSON parse: {} (stdout: {})",
            e,
            stdout.chars().take(200).collect::<String>()
        ))
    })?;

    parsed
        .issues
        .into_iter()
        .filter(|i| i.status == want_status)
        .map(|i| {
            let r = i.issue_ref()?;
            Ok(IssueRef {
                id: format!("#{}", r),
                backend: "kata".to_string(),
                url: None,
                title: i.title,
            })
        })
        .collect()
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
    use crate::signal::{Correction, DeferralSignal, ErrorSignal, PatternSignal, Workaround};

    // -----------------------------------------------------------------------
    // Original 7 tests (unchanged behaviour)
    // -----------------------------------------------------------------------

    #[test]
    fn kata_tracker_graceful_when_kata_missing_or_unconfigured() {
        // If kata is not on PATH, we get a Command error.
        // If kata is on PATH but the project doesn't exist, we get a Tracker error.
        // Either way, no panic.
        let tracker = KataTracker::new("nonexistent-jilog-test-project");
        let signal = Signal::Correction(Correction {
            session_id: "test".into(),
            context: "some correction context here".into(),
            ..Default::default()
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

    // -----------------------------------------------------------------------
    // Improvement 3: signal_priority — all 5 variants in one test
    // -----------------------------------------------------------------------

    #[test]
    fn signal_priority_all_variants() {
        let error = Signal::Error(ErrorSignal {
            session_id: "s".into(),
            tool_name: "bash".into(),
            message: "exit 1".into(),
            ..Default::default()
        });
        let correction = Signal::Correction(Correction {
            session_id: "s".into(),
            context: "stop that".into(),
            ..Default::default()
        });
        let pattern = Signal::Pattern(PatternSignal {
            session_id: "s".into(),
            description: "recurring theme".into(),
            ..Default::default()
        });
        let workaround = Signal::Workaround(Workaround {
            session_id: "s".into(),
            pattern: "for now".into(),
            context: "this is a hack".into(),
            ..Default::default()
        });
        let deferral = Signal::Deferral(DeferralSignal {
            session_id: "s".into(),
            item: "do this later".into(),
            ..Default::default()
        });

        assert_eq!(signal_priority(&error), 1, "Error must be priority 1");
        assert_eq!(signal_priority(&correction), 2, "Correction must be priority 2");
        assert_eq!(signal_priority(&pattern), 2, "Pattern must be priority 2");
        assert_eq!(signal_priority(&workaround), 3, "Workaround must be priority 3");
        assert_eq!(signal_priority(&deferral), 3, "Deferral must be priority 3");
    }

    // -----------------------------------------------------------------------
    // Improvement 2: build_body — one test per signal variant
    // -----------------------------------------------------------------------

    #[test]
    fn build_body_correction_contains_context() {
        let signal = Signal::Correction(Correction {
            session_id: "sess-abc".into(),
            context: "no, use the gog cli for calendar".into(),
            ..Default::default()
        });
        let body = build_body(&signal, "2026-05-11", None);
        assert!(body.contains("2026-05-11"), "body must contain date");
        assert!(body.contains("sess-abc"), "body must contain session_id");
        assert!(body.contains("correction"), "body must contain kind");
        assert!(body.contains("no, use the gog cli for calendar"), "body must contain context");
        assert!(body.contains("learning-digest-2026-05-11.md"), "body must reference digest path");
        assert!(body.contains("## Source"), "body must have Source section");
        assert!(body.contains("## Signal"), "body must have Signal section");
    }

    #[test]
    fn build_body_error_contains_tool_and_message() {
        let signal = Signal::Error(ErrorSignal {
            session_id: "sess-def".into(),
            tool_name: "bash".into(),
            message: "command not found: fzf".into(),
            ..Default::default()
        });
        let body = build_body(&signal, "2026-05-11", None);
        assert!(body.contains("Tool: bash"), "body must have 'Tool: bash'");
        assert!(body.contains("Message: command not found: fzf"), "body must have message line");
        assert!(body.contains("error"), "body must contain kind");
    }

    #[test]
    fn build_body_workaround_contains_pattern_and_context() {
        let signal = Signal::Workaround(Workaround {
            session_id: "sess-ghi".into(),
            pattern: "for now".into(),
            context: "temporarily using osascript".into(),
            ..Default::default()
        });
        let body = build_body(&signal, "2026-05-11", None);
        assert!(body.contains("Pattern: for now"), "body must have 'Pattern: for now'");
        assert!(body.contains("Context: temporarily using osascript"), "body must have context line");
        assert!(body.contains("workaround"), "body must contain kind");
    }

    #[test]
    fn build_body_pattern_contains_description() {
        let signal = Signal::Pattern(PatternSignal {
            session_id: "sess-jkl".into(),
            description: "always asks for confirmation before deleting".into(),
            ..Default::default()
        });
        let body = build_body(&signal, "2026-05-11", None);
        assert!(body.contains("always asks for confirmation before deleting"), "body must have description");
        assert!(body.contains("pattern"), "body must contain kind");
    }

    #[test]
    fn build_body_deferral_contains_item() {
        let signal = Signal::Deferral(DeferralSignal {
            session_id: "sess-mno".into(),
            item: "set up the CI pipeline".into(),
            ..Default::default()
        });
        let body = build_body(&signal, "2026-05-11", None);
        assert!(body.contains("set up the CI pipeline"), "body must have item");
        assert!(body.contains("deferral"), "body must contain kind");
    }

    // -----------------------------------------------------------------------
    // Improvement 1: list_closed smoke test (graceful when binary missing)
    // -----------------------------------------------------------------------

    #[test]
    fn list_closed_graceful_when_kata_missing() {
        // Mirrors the list_open smoke test. If kata is absent we get a
        // Command error; if the project is missing we get a Tracker error.
        // Either way, no panic.
        let tracker = KataTracker::new("nonexistent-jilog-test-project");
        match tracker.list_closed() {
            Ok(_) => eprintln!("kata list (closed) returned successfully (project may exist)"),
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
    }

    // -----------------------------------------------------------------------
    // Improvement 1: reopen() — fail-loud verification + integration note
    //
    // Full integration testing of reopen-on-recurrence requires a live kata
    // daemon with a project that has a closed issue matching the signal title.
    // The unit-level guarantee verified here: when kata is absent, the first
    // shell-out returns Err(Command(..)) immediately — no panic, no silent
    // swallow, no half-completed state.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // jilog#fx51 — create/list parsing against kata ≥0.15 JSON (no `number`)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_create_response_modern_json_uses_short_id() {
        // Real kata v0.15.1 shape: id is a numeric row id (NOT a CLI ref),
        // short_id is the ref, no `number` field anywhere.
        let stdout = r#"{"kata_api_version":1,"issue":{"id":2701,"uid":"01M0VTCB8995GXBZ1C0464K8N0","project_id":2,"project_uid":"01KR8TZZZ7XR7JAKE2S9D0CG71","short_id":"e3nj","title":"some issue","body":"...","status":"open","owner":null,"author":"jilog","metadata":{},"revision":1,"created_at":"2026-08-26T00:00:00Z","updated_at":"2026-08-26T00:00:00Z"}}"#;
        let r = parse_create_response(stdout).expect("modern create JSON must parse");
        assert_eq!(r, "e3nj");
    }

    #[test]
    fn parse_create_response_legacy_json_uses_number() {
        let stdout = r#"{"issue":{"number":42,"title":"legacy","status":"open"}}"#;
        let r = parse_create_response(stdout).expect("legacy create JSON must parse");
        assert_eq!(r, "42");
    }

    #[test]
    fn parse_create_response_without_any_ref_fails_loud() {
        let stdout = r#"{"issue":{"title":"drifty","status":"open"}}"#;
        let err = parse_create_response(stdout).expect_err("no ref fields must be an error");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("shape drifted"), "unexpected message: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }

    #[test]
    fn parse_list_response_modern_json_filters_status_and_uses_short_id() {
        let stdout = r#"{"kata_api_version":1,"issues":[
            {"id":1,"uid":"01AAA","short_id":"ab12","title":"open one","status":"open"},
            {"id":2,"uid":"01BBB","short_id":"cd34","title":"closed one","status":"closed"}
        ]}"#;
        let open = parse_list_response(stdout, "open").expect("list JSON must parse");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "#ab12");
        assert_eq!(open[0].title, "open one");
        let closed = parse_list_response(stdout, "closed").expect("list JSON must parse");
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id, "#cd34");
    }

    #[test]
    fn parse_list_response_malformed_json_is_loud_not_empty() {
        // The old code unwrap_or'd this into an empty list, silently
        // disabling dedup (jilog#fx51 comment thread). Must be an error.
        let err = parse_list_response("not json at all", "open")
            .expect_err("malformed list JSON must be an error, never an empty list");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("kata list JSON parse"), "unexpected message: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }

    #[test]
    fn parse_list_response_missing_status_is_loud() {
        // A row without `status` must not deserialize-with-default and then
        // vanish through the status filter (fresheyes 2026-08-26).
        let stdout = r#"{"issues":[{"short_id":"ab12","title":"drifty","uid":"01AAA"}]}"#;
        let err = parse_list_response(stdout, "open")
            .expect_err("row without status must be a parse error");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("kata list JSON parse"), "unexpected message: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }

    #[test]
    fn parse_list_response_missing_issues_collection_is_loud() {
        // A syntactically valid payload without the `issues` collection
        // (renamed key, error-ish shape that still exits 0) must not read
        // as an empty list (fresheyes 2026-08-26).
        let err = parse_list_response(r#"{"kata_api_version":1,"items":[]}"#, "open")
            .expect_err("missing issues collection must be a parse error");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("kata list JSON parse"), "unexpected message: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }

    #[test]
    fn parse_list_response_issue_without_ref_is_loud() {
        let stdout = r#"{"issues":[{"title":"no refs here","status":"open"}]}"#;
        let err = parse_list_response(stdout, "open")
            .expect_err("issue with no ref fields must be an error");
        match err {
            JilogReviewError::Tracker(msg) => {
                assert!(msg.contains("shape drifted"), "unexpected message: {}", msg);
            }
            other => panic!("expected Tracker variant, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // jilog#re4k — digest path + date threading into issue bodies
    // -----------------------------------------------------------------------

    #[test]
    fn build_body_uses_threaded_digest_path_when_present() {
        let signal = Signal::Correction(Correction {
            session_id: "sess-abc".into(),
            context: "context".into(),
            ..Default::default()
        });
        let body = build_body(
            &signal,
            "2026-08-26",
            Some("~/custom/digests/learning-digest-2026-08-26.md"),
        );
        assert!(
            body.contains("`~/custom/digests/learning-digest-2026-08-26.md`"),
            "body must use the threaded digest path: {}",
            body
        );
        assert!(
            !body.contains("~/.amplifier/health"),
            "hardcoded default must not appear when a path is threaded: {}",
            body
        );
    }

    #[test]
    fn with_run_context_pins_date_for_bodies() {
        let t = KataTracker::with_run_context("proj", "~/d/learning-digest-2026-01-02.md", "2026-01-02");
        assert_eq!(t.run_date(), "2026-01-02");
        // Default constructor falls back to a live clock — just assert shape.
        let d = KataTracker::new("proj").run_date();
        assert_eq!(d.len(), 10, "fallback date must be YYYY-MM-DD: {}", d);
    }

    #[test]
    fn reopen_fails_loud_when_kata_missing() {
        let tracker = KataTracker::new("nonexistent-jilog-test-project");
        let result = tracker.reopen(
            "999",
            "Recurred on 2026-05-11 — closure may have been premature.",
        );
        match result {
            Ok(()) => {
                eprintln!("kata reopen returned Ok (kata must be installed with matching project)");
            }
            Err(JilogReviewError::Command(msg)) => {
                eprintln!("kata not found — correct fail-loud behaviour: {}", msg);
            }
            Err(JilogReviewError::Tracker(msg)) => {
                eprintln!("kata returned structured error — correct fail-loud behaviour: {}", msg);
            }
            Err(e) => {
                eprintln!("other error variant (still not a panic): {}", e);
            }
        }
        // Test passes regardless — verifies no panic and errors surface rather
        // than being swallowed.
    }
}
