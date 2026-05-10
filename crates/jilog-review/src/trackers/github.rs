//! GithubTracker — shells out to the `gh` CLI.

use std::process::Command;

use serde::Deserialize;

use crate::error::JilogReviewError;
use crate::signal::Signal;
use crate::tracker::{IssueRef, Tracker, signal_title};

/// Tracker backed by GitHub Issues via the `gh` CLI.
///
/// Requires `gh` to be on PATH and authenticated (`gh auth login`).
pub struct GithubTracker {
    /// GitHub repository in `owner/repo` format.
    pub repo: String,
}

impl GithubTracker {
    pub fn new(repo: impl Into<String>) -> Self {
        Self { repo: repo.into() }
    }
}

/// Minimal schema for `gh issue list --json number,title,url`.
#[derive(Debug, Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    url: String,
}

/// Minimal schema for `gh issue view --json state`.
#[derive(Debug, Deserialize)]
struct GhIssueState {
    state: String,
}

impl Tracker for GithubTracker {
    fn name(&self) -> &str {
        "github"
    }

    fn create(&self, signal: &Signal) -> Result<IssueRef, JilogReviewError> {
        let title = signal_title(signal);

        // Dedup: return existing open issue if title matches.
        let open = self.list_open()?;
        if let Some(existing) = open.iter().find(|i| i.title == title) {
            return Ok(existing.clone());
        }

        let body = format!(
            "Detected by jilog review pipeline.\n\nSession: {}\nKind: {}",
            signal.session_id(),
            signal.kind()
        );
        let label = format!("jilog,jilog/{}", signal.kind());

        let output = Command::new("gh")
            .args([
                "issue", "create",
                "--repo", &self.repo,
                "--title", &title,
                "--body", &body,
                "--label", &label,
            ])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("gh issue create failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(JilogReviewError::Tracker(format!(
                "gh issue create non-zero exit: {}",
                stderr.trim()
            )));
        }

        // stdout is the issue URL on success.
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let id = parse_issue_id_from_url(&url)
            .unwrap_or_else(|| url.clone());

        Ok(IssueRef {
            id,
            backend: "github".to_string(),
            url: Some(url),
            title,
        })
    }

    fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
        let output = Command::new("gh")
            .args([
                "issue", "list",
                "--repo", &self.repo,
                "--state", "open",
                "--label", "jilog",
                "--json", "number,title,url",
            ])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("gh issue list failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(JilogReviewError::Tracker(format!(
                "gh issue list non-zero exit: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let issues: Vec<GhIssue> = serde_json::from_str(&stdout).unwrap_or_default();

        Ok(issues
            .into_iter()
            .map(|i| IssueRef {
                id: format!("#{}", i.number),
                backend: "github".to_string(),
                url: Some(i.url),
                title: i.title,
            })
            .collect())
    }

    fn is_resolved(&self, issue: &IssueRef) -> Result<bool, JilogReviewError> {
        // Strip leading '#' from id if present.
        let id = issue.id.trim_start_matches('#');

        let output = Command::new("gh")
            .args([
                "issue", "view",
                id,
                "--repo", &self.repo,
                "--json", "state",
            ])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("gh issue view failed: {}", e)))?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let state: GhIssueState = serde_json::from_str(&stdout).unwrap_or(GhIssueState {
            state: String::new(),
        });
        Ok(state.state == "CLOSED")
    }
}

/// Extract `#<number>` from a GitHub issue URL.
/// e.g. `https://github.com/owner/repo/issues/42` → `#42`
fn parse_issue_id_from_url(url: &str) -> Option<String> {
    url.split('/').next_back().map(|n| format!("#{}", n))
}
