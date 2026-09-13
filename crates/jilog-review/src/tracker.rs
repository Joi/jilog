//! Tracker trait — pluggable issue-tracker abstraction.

use crate::error::JilogReviewError;
use crate::signal::Signal;
use crate::util::truncate_chars;

// ---------------------------------------------------------------------------
// IssueRef
// ---------------------------------------------------------------------------

/// A reference to an issue in an external tracker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IssueRef {
    /// Tracker-specific issue ID (e.g. "opsctl-abc", "#42", "digest-1234").
    pub id: String,
    /// Which backend created this ref ("kata", "github", "none").
    pub backend: String,
    /// Optional URL to the issue.
    pub url: Option<String>,
    /// Issue title (matches signal_title output for dedup).
    pub title: String,
}

// ---------------------------------------------------------------------------
// Tracker trait
// ---------------------------------------------------------------------------

/// Pluggable issue tracker.
///
/// Implementations MUST dedup: `create()` checks `list_open()` for an
/// issue whose title matches `signal_title(signal)` before creating.
pub trait Tracker: Send + Sync {
    /// Stable name for this backend (e.g. "kata", "github", "none").
    fn name(&self) -> &str;

    /// Create an issue for `signal`, or return the existing one if a
    /// matching title is already open.
    fn create(&self, signal: &Signal) -> Result<IssueRef, JilogReviewError>;

    /// List all currently-open issues managed by this tracker.
    fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError>;

    /// Return true if the issue has been resolved (closed).
    fn is_resolved(&self, issue: &IssueRef) -> Result<bool, JilogReviewError>;
}

// ---------------------------------------------------------------------------
// signal_title — deterministic title for dedup
// ---------------------------------------------------------------------------

/// Build a deterministic issue title for dedup.
///
/// Format:
/// - Correction:  `[jilog/correction] <session_id>: <truncated context>`
/// - Error:       `[jilog/error] <tool_name>: <truncated message>`
/// - Workaround:  `[jilog/workaround] <pattern>: <truncated context>`
/// - Pattern:     `[jilog/pattern] <session_id>: <truncated description>`
/// - Deferral:    `[jilog/deferral] <session_id>: <truncated item>`
pub fn signal_title(signal: &Signal) -> String {
    match signal {
        Signal::Correction(c) => format!(
            "[jilog/correction] {}: {}",
            c.session_id,
            truncate_chars(&c.context, 80)
        ),
        Signal::Error(e) if e.tool_name == "codex_fallback_main" => {
            format!("[jilog/error] {}: {}", e.tool_name, e.session_id)
        }
        Signal::Error(e) => format!(
            "[jilog/error] {}: {}",
            e.tool_name,
            truncate_chars(&e.message, 80)
        ),
        Signal::Workaround(w) => format!(
            "[jilog/workaround] {}: {}",
            w.pattern,
            truncate_chars(&w.context, 80)
        ),
        Signal::Pattern(p) => format!(
            "[jilog/pattern] {}: {}",
            p.session_id,
            truncate_chars(&p.description, 80)
        ),
        Signal::Deferral(d) => format!(
            "[jilog/deferral] {}: {}",
            d.session_id,
            truncate_chars(&d.item, 80)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_allocation_title_is_stable_as_daily_count_grows() {
        let mut error = crate::signal::ErrorSignal {
            session_id: "allocation:host:2026-09-13:codex_fallback_main".into(),
            tool_name: "codex_fallback_main".into(),
            message: "1 row".into(),
            ..Default::default()
        };
        let first = signal_title(&Signal::Error(error.clone()));
        error.message = "25 rows".into();
        assert_eq!(first, signal_title(&Signal::Error(error)));
    }
}
