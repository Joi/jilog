//! Digest rendering and the top-level run_review orchestrator.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{NaiveDate, DateTime, Utc};
use rust_decimal::Decimal;

use crate::detectors::MAX_ERROR_MESSAGE_LENGTH;
use crate::detectors::{
    detect_corrections, detect_deferrals, detect_errors, detect_p0_alerts, detect_workarounds,
};
use crate::error::JilogReviewError;
use crate::health::detect_health_patterns;
use crate::reader::{ProcessedSessions, Reader};
use crate::signal::{Correction, DeferralSignal, ErrorSignal, PatternSignal, Signal, Workaround};
use crate::tracker::{IssueRef, Tracker, signal_title};
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
    /// Health patterns from readers with an event stream (see [`crate::health`]).
    pub patterns: Vec<PatternSignal>,
    pub p0_alerts: HashMap<String, BTreeSet<String>>,
    /// Aggregated spend across sessions that reported stats; None when no
    /// scanned session carried usage data.
    pub spend: Option<SpendSummary>,
    pub digest_path: PathBuf,
    pub created_issues: Vec<IssueRef>,
    pub sessions_scanned: usize,
}

/// Spend observed across the scanned sessions, aggregated from each
/// reader's [`crate::reader::SessionStats`].
///
/// jilog reports spend it observed in session files; it does not fetch
/// prices, maintain rate tables, or reconcile with provider billing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpendSummary {
    /// Sum of all known session costs; None when no session carried a cost
    /// (e.g. only unpriced models ran).
    pub total_cost_usd: Option<Decimal>,
    /// Sessions that reported usage stats at all.
    pub sessions_with_stats: usize,
    /// Subset of those whose calls carried `cost_usd`.
    pub sessions_with_cost: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Role → summed cost. Root sessions (no role suffix) are keyed "(root)".
    pub role_costs: BTreeMap<String, Decimal>,
    /// Model → summed cost.
    pub model_costs: BTreeMap<String, Decimal>,
}

// ---------------------------------------------------------------------------
// run_review — top-level orchestrator
// ---------------------------------------------------------------------------

/// Top-level orchestrator:
/// iterate readers → discover transcripts → load messages (+ events) →
/// run detectors → dedup against tracker → create issues → render digest.
pub fn run_review(
    readers: &[Box<dyn Reader>],
    tracker: &dyn Tracker,
    args: &ReviewArgs,
) -> Result<DigestReport, JilogReviewError> {
    let mut all_corrections: Vec<Correction> = Vec::new();
    let mut all_errors: Vec<ErrorSignal> = Vec::new();
    let mut all_workarounds: Vec<Workaround> = Vec::new();
    let mut all_deferrals: Vec<DeferralSignal> = Vec::new();
    let mut all_patterns: Vec<PatternSignal> = Vec::new();
    let mut sessions_scanned: usize = 0;
    let mut created_issues: Vec<IssueRef> = Vec::new();
    let mut spend = SpendSummary::default();
    // session_id → known session cost, for recurrence annotations.
    let mut session_costs: HashMap<String, Decimal> = HashMap::new();

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
                Ok(msgs) => msgs,
                Err(e) => {
                    tracing::warn!("reader '{}' load failed for {}: {}", reader.name(), handle.session_id, e);
                    continue;
                }
            };

            // Optional richer event stream for health-pattern detection;
            // Ok(None) means the reader has messages only.
            let events = match reader.load_events(&handle) {
                Ok(evts) => evts.unwrap_or_default(),
                Err(e) => {
                    tracing::warn!(
                        "reader '{}' load_events failed for {}: {}",
                        reader.name(), handle.session_id, e
                    );
                    Vec::new()
                }
            };

            if messages.is_empty() && events.is_empty() {
                continue; // nothing to analyze
            }

            all_corrections.extend(detect_corrections(&messages, &handle.session_id));
            all_errors.extend(detect_errors(&messages, &handle.session_id));
            all_workarounds.extend(detect_workarounds(&messages, &handle.session_id));
            all_deferrals.extend(detect_deferrals(&messages, &handle.session_id));
            all_patterns.extend(detect_health_patterns(&events, &handle.session_id));

            // Optional usage/spend stats; Ok(None) means the source format
            // carries no usage data.
            match reader.load_stats(&handle) {
                Ok(Some(stats)) => {
                    if let Some(cost) = accumulate_stats(&mut spend, &stats, &handle.session_id) {
                        session_costs.insert(handle.session_id.clone(), cost);
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "reader '{}' load_stats failed for {}: {}",
                    reader.name(), handle.session_id, e
                ),
            }

            sessions_scanned += 1;

            if let Some(ref mut ps) = processed {
                ps.mark(&handle.session_id);
            }
        }
    }

    let p0_alerts = detect_p0_alerts(&all_errors);
    let spend = if spend.sessions_with_stats > 0 { Some(spend) } else { None };

    // Week-over-week cost annotation: a signal whose title was ALREADY open
    // in the tracker before this run is a recurrence. Annotate it with the
    // summed cost of the sessions it occurred in during this run, when that
    // cost is known. Snapshot must happen before the create loop below adds
    // this run's new filings.
    let recurrence_costs: HashMap<String, String> = if args.dry_run || session_costs.is_empty() {
        HashMap::new()
    } else {
        let pre_open_titles: HashSet<String> = match tracker.list_open() {
            Ok(list) => list.into_iter().map(|i| i.title).collect(),
            Err(e) => {
                tracing::warn!("tracker.list_open failed (no recurrence annotations): {}", e);
                HashSet::new()
            }
        };
        let all_signals = all_corrections
            .iter()
            .map(|c| Signal::Correction(c.clone()))
            .chain(all_errors.iter().map(|e| Signal::Error(e.clone())))
            .chain(all_workarounds.iter().map(|w| Signal::Workaround(w.clone())))
            .chain(all_patterns.iter().map(|p| Signal::Pattern(p.clone())));
        recurrence_cost_annotations(all_signals, &pre_open_titles, &session_costs)
    };

    // Create issues if requested; build index keyed by signal_title for
    // bidirectional digest annotations (improvement 4).
    let mut issue_index: HashMap<String, IssueRef> = HashMap::new();
    if args.create_issues && !args.dry_run {
        for correction in &all_corrections {
            let signal = Signal::Correction(correction.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => {
                    issue_index.insert(signal_title(&signal), issue_ref.clone());
                    created_issues.push(issue_ref);
                }
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
        for error in &all_errors {
            let signal = Signal::Error(error.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => {
                    issue_index.insert(signal_title(&signal), issue_ref.clone());
                    created_issues.push(issue_ref);
                }
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
        for workaround in &all_workarounds {
            let signal = Signal::Workaround(workaround.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => {
                    issue_index.insert(signal_title(&signal), issue_ref.clone());
                    created_issues.push(issue_ref);
                }
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
        for pattern in &all_patterns {
            let signal = Signal::Pattern(pattern.clone());
            match tracker.create(&signal) {
                Ok(issue_ref) => {
                    issue_index.insert(signal_title(&signal), issue_ref.clone());
                    created_issues.push(issue_ref);
                }
                Err(e) => tracing::warn!("tracker.create failed: {}", e),
            }
        }
    }

    // Render and write digest.
    let date_str = args.date.format("%Y-%m-%d").to_string();
    let digest_path = args.digest_dir.join(format!("learning-digest-{}.md", date_str));

    // Preserve an earlier-written digest for the same date when this run
    // has nothing new to record. Without this, a mid-day re-run that sees
    // only already-processed sessions (sessions_scanned == 0) would
    // overwrite the populated digest with an empty one. The signal lists
    // only contain THIS run's findings, not the prior run's, so we have
    // no way to "merge" — skipping the write is the conservative choice.
    let should_write = !args.dry_run
        && !(sessions_scanned == 0 && digest_path.exists());

    if should_write {
        write_digest(
            &date_str,
            &all_corrections,
            &all_errors,
            &all_workarounds,
            &all_deferrals,
            &all_patterns,
            &p0_alerts,
            spend.as_ref(),
            &recurrence_costs,
            &args.digest_dir,
            &issue_index,
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
        patterns: all_patterns,
        p0_alerts,
        spend,
        digest_path,
        created_issues,
        sessions_scanned,
    })
}

// ---------------------------------------------------------------------------
// render_digest — byte-for-byte compatible with opsctl (unaffected lines)
// ---------------------------------------------------------------------------

/// Render a learning-digest markdown string.
///
/// Output format is byte-compatible with the Python script and opsctl for
/// lines where no issue was filed. Consumers that grep these digests depend
/// on this exact format.
///
/// When `issue_index` contains an entry for a signal (keyed by its
/// `signal_title`), the bullet line for that signal is annotated with
/// ` (→ backend#N)` before the trailing newline. Lines for signals without
/// a matching IssueRef are byte-identical to the pre-improvement-4 output.
pub fn render_digest(
    date: &str,
    corrections: &[Correction],
    errors: &[ErrorSignal],
    workarounds: &[Workaround],
    deferrals: &[DeferralSignal],
    patterns: &[PatternSignal],
    p0_alerts: &HashMap<String, BTreeSet<String>>,
    spend: Option<&SpendSummary>,
    recurrence_costs: &HashMap<String, String>,
    issue_index: &HashMap<String, IssueRef>,
) -> String {
    let signals =
        corrections.len() + errors.len() + workarounds.len() + deferrals.len() + patterns.len();
    let mut buf = String::new();
    buf.push_str("---\n");
    buf.push_str(&format!("date: {}\n", date));
    buf.push_str(&format!("signals_captured: {}\n", signals));
    buf.push_str(&format!("p0_count: {}\n", p0_alerts.len()));
    buf.push_str(&format!("corrections: {}\n", corrections.len()));
    buf.push_str(&format!("errors: {}\n", errors.len()));
    buf.push_str(&format!("workarounds: {}\n", workarounds.len()));
    buf.push_str(&format!("deferrals: {}\n", deferrals.len()));
    buf.push_str(&format!("patterns: {}\n", patterns.len()));
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
            let annotation = line_annotations(
                issue_index,
                recurrence_costs,
                &signal_title(&Signal::Correction(c.clone())),
            );
            buf.push_str(&format!(
                "- `{}` — {}{}\n",
                c.session_id,
                python_repr(&c.context),
                annotation
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
            let annotation = line_annotations(
                issue_index,
                recurrence_costs,
                &signal_title(&Signal::Error(e.clone())),
            );
            buf.push_str(&format!(
                "- `{}` / `{}`: {}{}\n",
                e.session_id, e.tool_name, msg, annotation
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
            let annotation = line_annotations(
                issue_index,
                recurrence_costs,
                &signal_title(&Signal::Workaround(w.clone())),
            );
            buf.push_str(&format!(
                "- `{}` pattern=`{}`: {}{}\n",
                w.session_id,
                w.pattern,
                python_repr(&w.context),
                annotation
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

    // Patterns (session-health; see crate::health)
    buf.push_str("## Patterns\n\n");
    if patterns.is_empty() {
        buf.push_str("_No patterns detected._\n\n");
    } else {
        for p in patterns {
            let annotation = line_annotations(
                issue_index,
                recurrence_costs,
                &signal_title(&Signal::Pattern(p.clone())),
            );
            buf.push_str(&format!(
                "- `{}` kind=`{}`: {}{}\n",
                p.session_id, p.pattern_kind, p.evidence, annotation
            ));
        }
        buf.push('\n');
    }

    // Spend — rendered only when at least one session reported stats.
    // No empty section otherwise: message-only readers stay silent here.
    if let Some(sp) = spend {
        buf.push_str("## Spend\n\n");
        match &sp.total_cost_usd {
            Some(total) => buf.push_str(&format!(
                "- **Total**: {} across {} of {} session(s) with usage data\n",
                format_usd(total),
                sp.sessions_with_cost,
                sp.sessions_with_stats
            )),
            None => buf.push_str(&format!(
                "- **Total**: no cost data ({} session(s) with usage; unpriced models)\n",
                sp.sessions_with_stats
            )),
        }
        buf.push_str(&format!(
            "- **Tokens**: {} in / {} out\n",
            sp.input_tokens, sp.output_tokens
        ));
        buf.push('\n');
        if !sp.role_costs.is_empty() {
            buf.push_str("### Spend by role\n\n");
            for (role, cost) in &sp.role_costs {
                buf.push_str(&format!("- `{}`: {}\n", role, format_usd(cost)));
            }
            buf.push('\n');
        }
        if !sp.model_costs.is_empty() {
            buf.push_str("### Spend by model\n\n");
            for (model, cost) in &sp.model_costs {
                buf.push_str(&format!("- `{}`: {}\n", model, format_usd(cost)));
            }
            buf.push('\n');
        }
    }

    buf
}

// ---------------------------------------------------------------------------
// write_digest — convenience wrapper (creates dir + writes file)
// ---------------------------------------------------------------------------

/// Write a digest file to `<digest_dir>/learning-digest-<date>.md`.
///
/// `issue_index` is forwarded to `render_digest` for bidirectional linking
/// annotations. Pass `&HashMap::new()` when no tracker is active.
pub fn write_digest(
    date: &str,
    corrections: &[Correction],
    errors: &[ErrorSignal],
    workarounds: &[Workaround],
    deferrals: &[DeferralSignal],
    patterns: &[PatternSignal],
    p0_alerts: &HashMap<String, BTreeSet<String>>,
    spend: Option<&SpendSummary>,
    recurrence_costs: &HashMap<String, String>,
    digest_dir: &Path,
    issue_index: &HashMap<String, IssueRef>,
) -> Result<PathBuf, JilogReviewError> {
    std::fs::create_dir_all(digest_dir)?;
    let path = digest_dir.join(format!("learning-digest-{}.md", date));
    let body = render_digest(
        date, corrections, errors, workarounds, deferrals, patterns, p0_alerts,
        spend, recurrence_costs, issue_index,
    );
    std::fs::write(&path, body)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the `(→ backend#N)` annotation suffix for a digest bullet line.
///
/// Returns an empty string if `signal_key` is not in `issue_index` (so the
/// bullet is byte-identical to the pre-annotation format for that line).
fn issue_annotation(issue_index: &HashMap<String, IssueRef>, signal_key: &str) -> String {
    match issue_index.get(signal_key) {
        Some(issue) => {
            // IssueRef.id is "#N" for kata; strip the leading '#' to avoid "kata##N".
            let id_num = issue.id.trim_start_matches('#');
            format!(" (→ {}#{})", issue.backend, id_num)
        }
        None => String::new(),
    }
}

/// All annotation suffixes for a digest bullet line: the issue link (if the
/// signal was filed) plus the week-over-week cost annotation (if the signal
/// recurred and its sessions carried cost data). Empty when neither applies,
/// keeping unaffected lines byte-identical.
fn line_annotations(
    issue_index: &HashMap<String, IssueRef>,
    recurrence_costs: &HashMap<String, String>,
    signal_key: &str,
) -> String {
    let mut out = issue_annotation(issue_index, signal_key);
    if let Some(total) = recurrence_costs.get(signal_key) {
        out.push_str(&format!(" (recurred in sessions totaling {})", total));
    }
    out
}

/// Format a Decimal as dollars: at least two decimal places ($4.20, not
/// $4.2), but sub-cent precision is preserved, never rounded away ($0.0003).
fn format_usd(d: &Decimal) -> String {
    let mut d = *d;
    if d.scale() < 2 {
        d.rescale(2);
    }
    format!("${}", d)
}

/// Fold one session's [`SessionStats`] into the run-wide [`SpendSummary`].
/// Returns the session's parsed cost when it had one (for the recurrence
/// annotation's session-cost map).
fn accumulate_stats(
    spend: &mut SpendSummary,
    stats: &crate::reader::SessionStats,
    session_id: &str,
) -> Option<Decimal> {
    spend.sessions_with_stats += 1;
    spend.input_tokens += stats.input_tokens;
    spend.output_tokens += stats.output_tokens;

    for (model, cost_str) in &stats.model_costs {
        match Decimal::from_str(cost_str) {
            Ok(c) => *spend.model_costs.entry(model.clone()).or_insert(Decimal::ZERO) += c,
            Err(e) => tracing::warn!(
                "session {}: unparseable model cost '{}': {}",
                session_id, cost_str, e
            ),
        }
    }

    let cost_str = stats.cost_usd.as_deref()?;
    let cost = match Decimal::from_str(cost_str) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("session {}: unparseable cost_usd '{}': {}", session_id, cost_str, e);
            return None;
        }
    };
    spend.sessions_with_cost += 1;
    spend.total_cost_usd = Some(spend.total_cost_usd.unwrap_or(Decimal::ZERO) + cost);
    let role_key = stats.role.clone().unwrap_or_else(|| "(root)".to_string());
    *spend.role_costs.entry(role_key).or_insert(Decimal::ZERO) += cost;
    Some(cost)
}

/// Build the week-over-week cost annotations: signal_title → formatted sum
/// of the costs of the distinct sessions in which that (recurring) signal
/// occurred during this run. Titles not in `pre_open_titles` are not
/// recurrences; sessions without known cost contribute nothing, and a title
/// whose sessions carried no cost at all gets no annotation.
fn recurrence_cost_annotations(
    signals: impl Iterator<Item = Signal>,
    pre_open_titles: &HashSet<String>,
    session_costs: &HashMap<String, Decimal>,
) -> HashMap<String, String> {
    if pre_open_titles.is_empty() {
        return HashMap::new();
    }
    let mut title_sessions: HashMap<String, BTreeSet<String>> = HashMap::new();
    for signal in signals {
        let title = signal_title(&signal);
        if pre_open_titles.contains(&title) {
            title_sessions
                .entry(title)
                .or_default()
                .insert(signal.session_id().to_string());
        }
    }
    let mut out = HashMap::new();
    for (title, sessions) in title_sessions {
        let mut sum = Decimal::ZERO;
        let mut any = false;
        for sess in &sessions {
            if let Some(cost) = session_costs.get(sess) {
                sum += *cost;
                any = true;
            }
        }
        if any {
            out.insert(title, format_usd(&sum));
        }
    }
    out
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

    // Helper: empty issue index (no tracker active).
    fn no_issues() -> HashMap<String, IssueRef> {
        HashMap::new()
    }

    #[test]
    fn digest_frontmatter_has_counts() {
        let corrections = vec![Correction { session_id: "a".into(), context: "fix it".into() }];
        let body = render_digest("2026-04-30", &corrections, &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &no_issues());
        assert!(body.starts_with("---\n"));
        assert!(body.contains("date: 2026-04-30"));
        assert!(body.contains("signals_captured: 1"));
        assert!(body.contains("corrections: 1"));
        assert!(body.contains("errors: 0"));
        assert!(body.contains("deferrals: 0"));
    }

    #[test]
    fn digest_empty_sections_use_placeholder() {
        let body = render_digest("2026-04-30", &[], &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &no_issues());
        assert!(body.contains("_No P0 alerts._"));
        assert!(body.contains("_No corrections detected._"));
        assert!(body.contains("_No errors detected._"));
        assert!(body.contains("_No workarounds detected._"));
        assert!(body.contains("_No deferrals detected._"));
        assert!(body.contains("_No patterns detected._"));
    }

    #[test]
    fn digest_p0_includes_session_list() {
        let mut p0 = HashMap::new();
        let mut sessions = BTreeSet::new();
        sessions.insert("aaa".into());
        sessions.insert("bbb".into());
        sessions.insert("ccc".into());
        p0.insert("bash".into(), sessions);
        let body = render_digest("2026-04-30", &[], &[], &[], &[], &[], &p0, None, &HashMap::new(), &no_issues());
        assert!(body.contains("`bash` failed in 3 distinct sessions"));
        assert!(body.contains("aaa, bbb, ccc"));
    }

    #[test]
    fn digest_corrections_use_python_repr() {
        let corrections = vec![Correction {
            session_id: "abc".into(),
            context: "don't do that".into(),
        }];
        let body = render_digest("2026-04-30", &corrections, &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &no_issues());
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
        let body = render_digest("2026-04-30", &[], &errors, &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &no_issues());
        assert!(body.contains("[truncated]"));
    }

    #[test]
    fn digest_patterns_render_kind_and_evidence() {
        let patterns = vec![PatternSignal {
            session_id: "s1".into(),
            description: "compaction storm: 4 compactions within 10 minutes".into(),
            pattern_kind: "compaction_storm".into(),
            evidence: "4 compactions 09:01-09:08".into(),
        }];
        let body = render_digest("2026-07-05", &[], &[], &[], &[], &patterns, &HashMap::new(), None, &HashMap::new(), &no_issues());
        assert!(body.contains("signals_captured: 1"));
        assert!(body.contains("patterns: 1"));
        assert!(body.contains("## Patterns"));
        assert!(body.contains("- `s1` kind=`compaction_storm`: 4 compactions 09:01-09:08"));
    }

    #[test]
    fn digest_patterns_annotated_when_issue_filed() {
        let pattern = PatternSignal {
            session_id: "s1".into(),
            description: "stuck loop: `bash` called 5 times with identical arguments".into(),
            pattern_kind: "stuck_loop".into(),
            evidence: "`bash` x5 identical arguments 09:00-09:04".into(),
        };
        let signal = Signal::Pattern(pattern.clone());
        let issue_ref = IssueRef {
            id: "#9".to_string(),
            backend: "kata".to_string(),
            url: None,
            title: signal_title(&signal),
        };
        let mut index = HashMap::new();
        index.insert(signal_title(&signal), issue_ref);

        let body = render_digest("2026-07-05", &[], &[], &[], &[], &[pattern], &HashMap::new(), None, &HashMap::new(), &index);
        assert!(body.contains("(→ kata#9)"), "annotation missing in:\n{}", body);
    }

    #[test]
    fn digest_deferrals_render_item() {
        let deferrals = vec![DeferralSignal {
            session_id: "s1".into(),
            item: "next session".into(),
        }];
        let body = render_digest("2026-04-30", &[], &[], &[], &deferrals, &[], &HashMap::new(), None, &HashMap::new(), &no_issues());
        assert!(body.contains("signals_captured: 1"));
        assert!(body.contains("- `s1` pattern=`next session`"));
    }

    #[test]
    fn write_digest_creates_file() {
        let dir = test_dir("digest-write");
        let path = write_digest("2026-04-30", &[], &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &dir, &no_issues()).unwrap();
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap(), "learning-digest-2026-04-30.md");
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Mid-day re-run preservation: a second run that finds zero new sessions
    // must NOT overwrite a digest written by an earlier run for the same
    // date.
    // -----------------------------------------------------------------------

    #[test]
    fn run_review_preserves_existing_digest_when_nothing_scanned() {
        use crate::trackers::NoneTracker;
        use chrono::{NaiveDate, Utc};

        let dir = test_dir("preserve");
        let date = NaiveDate::from_ymd_opt(2026, 5, 18).unwrap();
        let digest_path = dir.join("learning-digest-2026-05-18.md");

        // Seed: a populated digest from an earlier run.
        let seeded = "---\ndate: 2026-05-18\nsignals_captured: 99\n---\nSEEDED-FROM-EARLIER-RUN\n";
        fs::write(&digest_path, seeded).unwrap();

        // Re-run with zero readers (sessions_scanned = 0) on the same date.
        let readers: Vec<Box<dyn Reader>> = Vec::new();
        let tracker = NoneTracker;
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: None,
            date,
            dry_run: false,
            create_issues: false,
        };
        let report = run_review(&readers, &tracker, &args).unwrap();
        assert_eq!(report.sessions_scanned, 0);

        // Critical: the seeded content must still be there.
        let body = fs::read_to_string(&digest_path).unwrap();
        assert!(
            body.contains("SEEDED-FROM-EARLIER-RUN"),
            "mid-day re-run with 0 sessions overwrote the populated digest"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_review_writes_digest_when_file_absent_even_with_no_sessions() {
        use crate::trackers::NoneTracker;
        use chrono::{NaiveDate, Utc};

        let dir = test_dir("first-empty");
        let date = NaiveDate::from_ymd_opt(2026, 5, 18).unwrap();
        let digest_path = dir.join("learning-digest-2026-05-18.md");
        assert!(!digest_path.exists());

        let readers: Vec<Box<dyn Reader>> = Vec::new();
        let tracker = NoneTracker;
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: None,
            date,
            dry_run: false,
            create_issues: false,
        };
        run_review(&readers, &tracker, &args).unwrap();

        // First run of the day with nothing to scan still produces a
        // digest skeleton, so downstream tooling can grep for today's file.
        assert!(digest_path.exists(), "first run of the day should write the empty digest");
        let body = fs::read_to_string(&digest_path).unwrap();
        assert!(body.contains("signals_captured: 0"));
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Spend section (W3)
    // -----------------------------------------------------------------------

    #[test]
    fn digest_spend_section_renders_totals_roles_models() {
        let spend = SpendSummary {
            total_cost_usd: Some(Decimal::from_str("4.2").unwrap()),
            sessions_with_stats: 3,
            sessions_with_cost: 2,
            input_tokens: 12345,
            output_tokens: 678,
            role_costs: BTreeMap::from([
                ("(root)".to_string(), Decimal::from_str("3.1").unwrap()),
                ("explore".to_string(), Decimal::from_str("1.1").unwrap()),
            ]),
            model_costs: BTreeMap::from([
                ("claude-opus-4-8".to_string(), Decimal::from_str("4.2").unwrap()),
            ]),
        };
        let body = render_digest(
            "2026-07-05", &[], &[], &[], &[], &[], &HashMap::new(),
            Some(&spend), &HashMap::new(), &no_issues(),
        );
        assert!(body.contains("## Spend"));
        assert!(body.contains("- **Total**: $4.20 across 2 of 3 session(s) with usage data"));
        assert!(body.contains("- **Tokens**: 12345 in / 678 out"));
        assert!(body.contains("### Spend by role"));
        assert!(body.contains("- `(root)`: $3.10"));
        assert!(body.contains("- `explore`: $1.10"));
        assert!(body.contains("### Spend by model"));
        assert!(body.contains("- `claude-opus-4-8`: $4.20"));
    }

    #[test]
    fn digest_spend_section_absent_without_stats() {
        let body = render_digest(
            "2026-07-05", &[], &[], &[], &[], &[], &HashMap::new(),
            None, &HashMap::new(), &no_issues(),
        );
        assert!(!body.contains("## Spend"), "no stats → no Spend section:\n{}", body);
    }

    #[test]
    fn digest_spend_all_unpriced_says_no_cost_data() {
        let spend = SpendSummary {
            total_cost_usd: None,
            sessions_with_stats: 2,
            sessions_with_cost: 0,
            input_tokens: 10,
            output_tokens: 1,
            ..Default::default()
        };
        let body = render_digest(
            "2026-07-05", &[], &[], &[], &[], &[], &HashMap::new(),
            Some(&spend), &HashMap::new(), &no_issues(),
        );
        assert!(body.contains("- **Total**: no cost data (2 session(s) with usage; unpriced models)"));
        assert!(!body.contains("### Spend by role"), "no costs → no role table");
    }

    #[test]
    fn format_usd_pads_cents_but_keeps_subcent_precision() {
        assert_eq!(format_usd(&Decimal::from_str("4.2").unwrap()), "$4.20");
        assert_eq!(format_usd(&Decimal::from_str("7").unwrap()), "$7.00");
        assert_eq!(format_usd(&Decimal::from_str("0.0003").unwrap()), "$0.0003");
        assert_eq!(format_usd(&Decimal::from_str("4.20").unwrap()), "$4.20");
    }

    // -----------------------------------------------------------------------
    // Week-over-week recurrence cost annotation (W3)
    // -----------------------------------------------------------------------

    use crate::reader::{Message, SessionStats, TranscriptHandle};

    /// One-session reader with canned messages and stats.
    struct FixtureReader {
        session_id: String,
        messages: Vec<Message>,
        stats: Option<SessionStats>,
    }

    impl Reader for FixtureReader {
        fn name(&self) -> &str {
            "fixture"
        }
        fn discover(&self, _since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
            Ok(vec![TranscriptHandle {
                session_id: self.session_id.clone(),
                path: PathBuf::from("/nonexistent/fixture.jsonl"),
                modified: Utc::now(),
                reader_name: "fixture".to_string(),
            }])
        }
        fn load(&self, _handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
            Ok(self.messages.clone())
        }
        fn load_stats(
            &self,
            _handle: &TranscriptHandle,
        ) -> Result<Option<SessionStats>, JilogReviewError> {
            Ok(self.stats.clone())
        }
    }

    /// Tracker that already has the given titles open (and refuses creates).
    struct OpenTitlesTracker {
        titles: Vec<String>,
    }

    impl Tracker for OpenTitlesTracker {
        fn name(&self) -> &str {
            "mock"
        }
        fn create(&self, _signal: &Signal) -> Result<IssueRef, JilogReviewError> {
            Err(JilogReviewError::Tracker("create not expected in this test".into()))
        }
        fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
            Ok(self
                .titles
                .iter()
                .map(|t| IssueRef {
                    id: "#1".to_string(),
                    backend: "mock".to_string(),
                    url: None,
                    title: t.clone(),
                })
                .collect())
        }
        fn is_resolved(&self, _issue: &IssueRef) -> Result<bool, JilogReviewError> {
            Ok(false)
        }
    }

    fn correction_messages(context: &str) -> Vec<Message> {
        let msg = |role: &str, text: &str| Message {
            role: Some(role.to_string()),
            content: Some(serde_json::Value::String(text.to_string())),
            name: None,
        };
        vec![msg("assistant", "first"), msg("user", context), msg("assistant", "second")]
    }

    #[test]
    fn run_review_annotates_recurring_signal_with_session_cost() {
        let dir = test_dir("recurrence-cost");
        let context = "no, use the gog cli for calendar";
        let correction = Correction {
            session_id: "sess-r_explore".into(),
            context: context.into(),
        };
        let already_open = signal_title(&Signal::Correction(correction));

        let readers: Vec<Box<dyn Reader>> = vec![Box::new(FixtureReader {
            session_id: "sess-r_explore".into(),
            messages: correction_messages(context),
            stats: Some(SessionStats {
                cost_usd: Some("4.2".into()),
                input_tokens: 100,
                output_tokens: 10,
                role: Some("explore".into()),
                model_costs: BTreeMap::new(),
            }),
        })];
        let tracker = OpenTitlesTracker { titles: vec![already_open] };
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: None,
            date: NaiveDate::from_ymd_opt(2026, 7, 5).unwrap(),
            dry_run: false,
            create_issues: false,
        };
        let report = run_review(&readers, &tracker, &args).unwrap();
        assert_eq!(report.corrections.len(), 1);
        let spend = report.spend.as_ref().expect("stats were provided");
        assert_eq!(spend.total_cost_usd, Some(Decimal::from_str("4.2").unwrap()));
        assert_eq!(spend.role_costs.get("explore"), Some(&Decimal::from_str("4.2").unwrap()));

        let body = fs::read_to_string(&report.digest_path).unwrap();
        assert!(
            body.contains("(recurred in sessions totaling $4.20)"),
            "recurrence annotation missing:\n{}",
            body
        );
        assert!(body.contains("## Spend"), "spend section missing:\n{}", body);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_review_no_recurrence_annotation_for_new_signals() {
        let dir = test_dir("recurrence-none");
        let readers: Vec<Box<dyn Reader>> = vec![Box::new(FixtureReader {
            session_id: "sess-n".into(),
            messages: correction_messages("please stop doing that thing"),
            stats: Some(SessionStats {
                cost_usd: Some("1.0".into()),
                input_tokens: 1,
                output_tokens: 1,
                role: None,
                model_costs: BTreeMap::new(),
            }),
        })];
        // Tracker knows about some OTHER title only.
        let tracker = OpenTitlesTracker { titles: vec!["[jilog/error] other: thing".into()] };
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: None,
            date: NaiveDate::from_ymd_opt(2026, 7, 5).unwrap(),
            dry_run: false,
            create_issues: false,
        };
        let report = run_review(&readers, &tracker, &args).unwrap();
        let body = fs::read_to_string(&report.digest_path).unwrap();
        assert!(!body.contains("recurred in sessions totaling"), "digest:\n{}", body);
        // Root session cost lands under "(root)".
        assert!(body.contains("- `(root)`: $1.00"), "digest:\n{}", body);
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Improvement 4: bidirectional linking annotation
    // -----------------------------------------------------------------------

    #[test]
    fn digest_annotation_appended_to_correction_bullet() {
        let correction = Correction {
            session_id: "0e91a2b4".into(),
            context: "no, use the gog cli for calendar".into(),
        };
        let signal = Signal::Correction(correction.clone());
        let issue_ref = IssueRef {
            id: "#7".to_string(),
            backend: "kata".to_string(),
            url: None,
            title: signal_title(&signal),
        };
        let mut index = HashMap::new();
        index.insert(signal_title(&signal), issue_ref);

        let body = render_digest("2026-05-11", &[correction], &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &index);
        // Annotation must appear at end of bullet line, before newline.
        assert!(
            body.contains("(→ kata#7)"),
            "annotation missing in:\n{}", body
        );
        // Must not produce double-hash.
        assert!(!body.contains("kata##"), "double-hash found in:\n{}", body);
    }

    #[test]
    fn digest_no_annotation_when_issue_index_empty() {
        let correction = Correction {
            session_id: "abc".into(),
            context: "fix it".into(),
        };
        let body = render_digest("2026-05-11", &[correction], &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &no_issues());
        // Line must end with content then newline — no trailing annotation.
        assert!(!body.contains("(→"), "unexpected annotation in:\n{}", body);
    }

    #[test]
    fn digest_annotation_byte_stable_for_unaffected_lines() {
        // A correction WITH an annotation and one WITHOUT — unaffected line
        // must be byte-identical to the no-annotation render.
        let c_annotated = Correction { session_id: "ann".into(), context: "do this".into() };
        let c_plain = Correction { session_id: "pla".into(), context: "plain line".into() };

        let signal_annotated = Signal::Correction(c_annotated.clone());
        let issue_ref = IssueRef {
            id: "#3".to_string(),
            backend: "kata".to_string(),
            url: None,
            title: signal_title(&signal_annotated),
        };
        let mut index = HashMap::new();
        index.insert(signal_title(&signal_annotated), issue_ref);

        let body_with = render_digest(
            "2026-05-11",
            &[c_annotated.clone(), c_plain.clone()],
            &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &index,
        );
        let body_without = render_digest(
            "2026-05-11",
            &[c_annotated.clone(), c_plain.clone()],
            &[], &[], &[], &[], &HashMap::new(), None, &HashMap::new(), &no_issues(),
        );

        // The plain line must be identical in both renders.
        let plain_line_with = body_with.lines().find(|l| l.contains("plain line")).unwrap();
        let plain_line_without = body_without.lines().find(|l| l.contains("plain line")).unwrap();
        assert_eq!(plain_line_with, plain_line_without, "unaffected line changed");

        // The annotated line must differ (has the annotation).
        let ann_line_with = body_with.lines().find(|l| l.contains("do this")).unwrap();
        let ann_line_without = body_without.lines().find(|l| l.contains("do this")).unwrap();
        assert_ne!(ann_line_with, ann_line_without, "annotated line should differ");
        assert!(ann_line_with.ends_with("(→ kata#3)"), "annotation must be at end of line");
    }
}
