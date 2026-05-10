//! NoneTracker — a no-op tracker that returns synthetic IssueRefs.
//!
//! Useful when you want to run the review pipeline and render a digest
//! without writing to any external issue tracker.

use crate::error::JilogReviewError;
use crate::signal::Signal;
use crate::tracker::{IssueRef, Tracker, signal_title};

/// No-op tracker. Returns synthetic IssueRefs with backend="none".
pub struct NoneTracker;

impl Tracker for NoneTracker {
    fn name(&self) -> &str {
        "none"
    }

    fn create(&self, signal: &Signal) -> Result<IssueRef, JilogReviewError> {
        // Return synthetic IssueRef so the digest still records "would have created".
        Ok(IssueRef {
            id: format!("digest-{}", chrono::Utc::now().timestamp()),
            backend: "none".to_string(),
            url: None,
            title: signal_title(signal),
        })
    }

    fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
        Ok(vec![])
    }

    fn is_resolved(&self, _issue: &IssueRef) -> Result<bool, JilogReviewError> {
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::Correction;

    #[test]
    fn none_tracker_create_returns_synthetic_ref() {
        let tracker = NoneTracker;
        let signal = Signal::Correction(Correction {
            session_id: "sess-1".into(),
            context: "please fix this".into(),
        });
        let issue = tracker.create(&signal).unwrap();
        assert_eq!(issue.backend, "none");
        assert!(issue.id.starts_with("digest-"));
        assert!(issue.url.is_none());
        // Title format: [jilog/correction] sess-1: please fix this
        assert!(issue.title.contains("jilog/correction"));
    }

    #[test]
    fn none_tracker_list_open_empty() {
        let tracker = NoneTracker;
        assert!(tracker.list_open().unwrap().is_empty());
    }

    #[test]
    fn none_tracker_is_resolved_false() {
        let tracker = NoneTracker;
        let issue = IssueRef {
            id: "digest-1".into(),
            backend: "none".into(),
            url: None,
            title: "test".into(),
        };
        assert!(!tracker.is_resolved(&issue).unwrap());
    }
}
