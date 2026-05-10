//! Digest rendering and the top-level run_review orchestrator.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, DateTime, Utc};

use crate::detectors::MAX_ERROR_MESSAGE_LENGTH;
use crate::detectors::{
    detect_corrections, detect_deferrals, detect_errors, detect_p0_alerts, detect_workarounds,
};
use crate::error::JilogReviewError;
use crate::reader::{ProcessedSessions, Reader};
use crate::signal::{Correction, DeferralSignal, ErrorSignal, Signal, Workaround};
use crate::tracker::{IssueRef, Tracker};
use crate::util::{python_repr, truncate_with_marker};

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Arguments for run_review.
pub struct ReviewArgs {
    /// Only process transcripts modified at-or-after this timestamp.
    pub since: DateTime<Utc>,
    /// Directory where the markdown digest is written.
    pub digest_dir: PathBuf,
    /// Path to the processed-sessions dedup file (None = no dedup).
    pub processed_file: Option<PathBuf>,
    /// Date stamp embedded in the digest filename and frontmatter.
    pub date: NaiveDate,
    /// If true, skip writing files and creating issues.
    pub dry_run: bool,
    /// If true (and not dry_run), create issues in the tracker.
    pub create_issues: bool,
}

/// Result of a run_review call.
pub struct DigestReport {
    pub date: NaiveDate,
    pub corrections: Vec<Correction>,
    pub errors: Vec<ErrorSignal>,
    pub workarounds: Vec<Workaround>,
    pub deferrals: Vec<DeferralSignal>,
    pub p0_alerts: HashMap<String, BTreeSet<String>>,
    pub digest_path: PathBuf,
    pub created_issues: Vec<IssueRef>,
    pub sessions_scanned: usize,
}

// ---------------------------------------------------------------------------
// run_review — top-level orchestrator
// ---------------------------------------------------------------------------

/// Top-level orchestrator:
/// iterate readers → discover transcripts → load messages →
/// run detectors → dedup against tracker → create issues → render digest.
///
/// Note: Pattern signals are produced by NO detector at this time. They are in
/// the Signal enum for forward-compatibility only.
pub fn run_review(
    readers: &[Box<dyn Reader>],
    tracker: &dyn Tracker,
    args: &ReviewArgs,
) -> Result<DigestReport, JilogReviewError> {
    let mut all_corrections: Vec<Correction> = Vec::new();
    let mut all_errors: Vec<ErrorSignal> = Vec::new();
    let mut all_workarounds: Vec<Workaround> = Vec::new();
    let mut all_deferrals: Vec<DeferralSignal> = Vec::new();
    let mut sessions_scanned: usize = 0;
    let mut created_issues: Vec<IssueRef> = Vec::new();

    // Load processed-sessions dedup file if configured.
    let mut processed: Option<ProcessedSessions> = match &args.processed_file {
        Some(path) => Some(ProcessedSessions::load(path)?),
        None => None,
    };

    for reader in readers {
        let handles = match reader.discover(args.since) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("reader '{}' discover failed: {}", reader.name(), e);
                continue;
            }
        };

        for handle in handles {
            // Skip already-processed sessions.
            if let Some(ref ps) = processed {
                if ps.contains(&handle.session_id) {
                    continue;
                }
            }

            let messages = match reader.load(&handle) {
                Ok(msgs) if !msgs.is_empty() => msgs,
                Ok(_) => continue, // empty transcript
                Err(e) => {
                    tracing::warn!("reader '{}' load failed for {}: {}", reader.name(), handle.session_id, e);
                    continue;
                }
            };

            let corrections = detect_corrections(&messages, &handle.session_id);
            let errors = detect_errors(&messages, &handle.session_id);
            let workarounds = detect_workarounds(&messages, &handle.session_id);
            let deferrals = detect_deferrals(&messages, &handle.session_id);

            all_corrections.extend(corrections);
            all_errors.extend(errors);
            all_workarounds.extend(workarounds);
            all_deferrals.extend(deferrals);
            sessions_scanned += 1;

            if let Some(ref mut ps) = processed {
                ps.mark(&handle.session_id);
            }
        }
    }

    let p0_alerts = detect_p0_alerts(&all_errors);

    // Create issues if requested.
    if args.create_issues && !args.dry_run {
        for correction in &all_corrections {
            let signal = Signal::Correction(correction.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => created_issues.push(issue_ref),
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
        for error in &all_errors {
            let signal = Signal::Error(error.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => created_issues.push(issue_ref),
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
        for workaround in &all_workarounds {
            let signal = Signal::Workaround(workaround.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => created_issues.push(issue_ref),
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
        for deferral in &all_deferrals {
            let signal = Signal::Deferral(deferral.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => created_issues.push(issue_ref),
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
    }

    // Render and write digest.
    let date_str = args.date.format("%Y-%m-%d").to_string();
    let digest_path = args.digest_dir.join(format!("learning-digest-{}.md", date_str));

    if !args.dry_run {
        write_digest(
            &date_str,
            &all_corrections,
            &all_errors,
            &all_workarounds,
            &all_deferrals,
            &p0_alerts,
            &args.digest_dir,
        )?;
    }

    // Persist processed-sessions.
    if !args.dry_run {
        if let (Some(ref ps), Some(ref pf)) = (&processed, &args.processed_file) {
            ps.save(pf)?;
        }
    }

    Ok(DigestReport {
        date: args.date,
        corrections: all_corrections,
        errors: all_errors,
        workarounds: all_workarounds,
        deferrals: all_deferrals,
        p0_alerts,
        digest_path,
        created_issues,
        sessions_scanned,
    })
}

// ---------------------------------------------------------------------------
// render_digest — stable markdown digest format
// ---------------------------------------------------------------------------

/// Render a learning-digest markdown string.
///
/// Consumers that grep these digests depend on this exact format.
pub fn render_digest(
    date: &str,
    corrections: &[Correction],
    errors: &[ErrorSignal],
    workarounds: &[Workaround],
    deferrals: &[DeferralSignal],
    p0_alerts: &HashMap<String, BTreeSet<String>>,
) -> String {
    let signals = corrections.len() + errors.len() + workarounds.len() + deferrals.len();
    let mut buf = String::new();
    buf.push_str("---\n");
    buf.push_str(&format!("date: {}\n", date));
    buf.push_str(&format!("signals_captured: {}\n", signals));
    buf.push_str(&format!("p0_count: {}\n", p0_alerts.len()));
    buf.push_str(&format!("corrections: {}\n", corrections.len()));
    buf.push_str(&format!("errors: {}\n", errors.len()));
    buf.push_str(&format!("workarounds: {}\n", workarounds.len()));
    buf.push_str(&format!("deferrals: {}\n", deferrals.len()));
    buf.push_str("---\n\n");

    buf.push_str(&format!("# Learning Digest — {}\n\n", date));

    // P0 Alerts
    buf.push_str("## P0 Alerts\n\n");
    if p0_alerts.is_empty() {
        buf.push_str("_No P0 alerts._\n\n");
    } else {
        let mut tools: Vec<&String> = p0_alerts.keys().collect();
        tools.sort();
        for tool in tools {
            let sessions = &p0_alerts[tool];
            let session_list: Vec<&str> = sessions.iter().map(|s| s.as_str()).collect();
            buf.push_str(&format!(
                "- **P0 ALERT**: `{}` failed in {} distinct sessions: {}\n",
                tool,
                sessions.len(),
                session_list.join(", ")
            ));
        }
        buf.push('\n');
    }

    // Corrections
    buf.push_str("## Corrections\n\n");
    if corrections.is_empty() {
        buf.push_str("_No corrections detected._\n\n");
    } else {
        for c in corrections {
            buf.push_str(&format!(
                "- `{}` — {}\n",
                c.session_id,
                python_repr(&c.context)
            ));
        }
        buf.push('\n');
    }

    // Errors
    buf.push_str("## Errors\n\n");
    if errors.is_empty() {
        buf.push_str("_No errors detected._\n\n");
    } else {
        for e in errors {
            let msg = truncate_with_marker(&e.message, MAX_ERROR_MESSAGE_LENGTH);
            buf.push_str(&format!(
                "- `{}` / `{}`: {}\n",
                e.session_id, e.tool_name, msg
            ));
        }
        buf.push('\n');
    }

    // Workarounds
    buf.push_str("## Workarounds\n\n");
    if workarounds.is_empty() {
        buf.push_str("_No workarounds detected._\n\n");
    } else {
        for w in workarounds {
            buf.push_str(&format!(
                "- `{}` pattern=`{}`: {}\n",
                w.session_id,
                w.pattern,
                python_repr(&w.context)
            ));
        }
        buf.push('\n');
    }

    // Deferrals
    buf.push_str("## Deferrals\n\n");
    if deferrals.is_empty() {
        buf.push_str("_No deferrals detected._\n\n");
    } else {
        for d in deferrals {
            buf.push_str(&format!("- `{}` pattern=`{}`\n", d.session_id, d.item));
        }
        buf.push('\n');
    }

    buf
}

// ---------------------------------------------------------------------------
// write_digest — convenience wrapper (creates dir + writes file)
// ---------------------------------------------------------------------------

/// Write a digest file to `<digest_dir>/learning-digest-<date>.md`.
pub fn write_digest(
    date: &str,
    corrections: &[Correction],
    errors: &[ErrorSignal],
    workarounds: &[Workaround],
    deferrals: &[DeferralSignal],
    p0_alerts: &HashMap<String, BTreeSet<String>>,
    digest_dir: &Path,
) -> Result<PathBuf, JilogReviewError> {
    std::fs::create_dir_all(digest_dir)?;
    let path = digest_dir.join(format!("learning-digest-{}.md", date));
    let body = render_digest(date, corrections, errors, workarounds, deferrals, p0_alerts);
    std::fs::write(&path, body)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Tests — ported from opsctl/crates/opsctl/src/review_nightly.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("jilog-test-digest")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn digest_frontmatter_has_counts() {
        let corrections = vec![Correction { session_id: "a".into(), context: "fix it".into() }];
        let body = render_digest("2026-04-30", &corrections, &[], &[], &[], &HashMap::new());
        assert!(body.starts_with("---\n"));
        assert!(body.contains("date: 2026-04-30"));
        assert!(body.contains("signals_captured: 1"));
        assert!(body.contains("corrections: 1"));
        assert!(body.contains("errors: 0"));
        assert!(body.contains("deferrals: 0"));
    }

    #[test]
    fn digest_empty_sections_use_placeholder() {
        let body = render_digest("2026-04-30", &[], &[], &[], &[], &HashMap::new());
        assert!(body.contains("_No P0 alerts._"));
        assert!(body.contains("_No corrections detected._"));
        assert!(body.contains("_No errors detected._"));
        assert!(body.contains("_No workarounds detected._"));
        assert!(body.contains("_No deferrals detected._"));
    }

    #[test]
    fn digest_p0_includes_session_list() {
        let mut p0 = HashMap::new();
        let mut sessions = BTreeSet::new();
        sessions.insert("aaa".into());
        sessions.insert("bbb".into());
        sessions.insert("ccc".into());
        p0.insert("bash".into(), sessions);
        let body = render_digest("2026-04-30", &[], &[], &[], &[], &p0);
        assert!(body.contains("`bash` failed in 3 distinct sessions"));
        assert!(body.contains("aaa, bbb, ccc"));
    }

    #[test]
    fn digest_corrections_use_python_repr() {
        let corrections = vec![Correction {
            session_id: "abc".into(),
            context: "don't do that".into(),
        }];
        let body = render_digest("2026-04-30", &corrections, &[], &[], &[], &HashMap::new());
        // Single quote inside should be escaped: \'
        assert!(body.contains("'don\\'t do that'"), "digest body: {}", body);
    }

    #[test]
    fn digest_errors_truncated_with_marker() {
        let errors = vec![ErrorSignal {
            session_id: "s1".into(),
            tool_name: "bash".into(),
            message: "x".repeat(600),
        }];
        let body = render_digest("2026-04-30", &[], &errors, &[], &[], &HashMap::new());
        assert!(body.contains("[truncated]"));
    }

    #[test]
    fn digest_deferrals_render_item() {
        let deferrals = vec![DeferralSignal {
            session_id: "s1".into(),
            item: "next session".into(),
        }];
        let body = render_digest("2026-04-30", &[], &[], &[], &deferrals, &HashMap::new());
        assert!(body.contains("signals_captured: 1"));
        assert!(body.contains("- `s1` pattern=`next session`"));
    }

    #[test]
    fn write_digest_creates_file() {
        let dir = test_dir("digest-write");
        let path = write_digest("2026-04-30", &[], &[], &[], &[], &HashMap::new(), &dir).unwrap();
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap(), "learning-digest-2026-04-30.md");
        let _ = fs::remove_dir_all(&dir);
    }
}
