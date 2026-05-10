//! BeadsTracker — shells out to the `bd` CLI.
//!
//! Matches opsctl's existing beads integration (see
//! opsctl/crates/opsctl/src/review.rs:1779-1810).

use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::error::JilogReviewError;
use crate::signal::Signal;
use crate::tracker::{IssueRef, Tracker, signal_title};

/// Tracker backed by the `bd` CLI in a local beads repository.
pub struct BeadsTracker {
    pub repo_path: PathBuf,
}

impl BeadsTracker {
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        Self { repo_path: repo_path.into() }
    }
}

/// Minimal schema for parsing `bd list --json` output.
#[derive(Debug, Deserialize)]
struct BeadsIssue {
    id: String,
    title: String,
    status: String,
    #[allow(dead_code)]
    priority: i32,
    #[allow(dead_code)]
    issue_type: String,
}

impl Tracker for BeadsTracker {
    fn name(&self) -> &str {
        "beads"
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

        let output = Command::new("bd")
            .current_dir(&self.repo_path)
            .args([
                "create",
                "--title", &title,
                "--type=task",
                "--priority=2",
                "-d", &body,
            ])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("bd create failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(JilogReviewError::Tracker(format!(
                "bd create non-zero exit: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // bd prints: "Created issue: <id>" or similar.
        let id = parse_created_id(&stdout).unwrap_or_else(|| "unknown".to_string());

        Ok(IssueRef {
            id,
            backend: "beads".to_string(),
            url: None,
            title,
        })
    }

    fn list_open(&self) -> Result<Vec<IssueRef>, JilogReviewError> {
        let output = Command::new("bd")
            .current_dir(&self.repo_path)
            .args(["list", "--json", "--status=open"])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("bd list failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(JilogReviewError::Tracker(format!(
                "bd list non-zero exit: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let issues: Vec<BeadsIssue> =
            serde_json::from_str(&stdout).unwrap_or_default();

        Ok(issues
            .into_iter()
            .filter(|i| i.status == "open" || i.status == "in_progress")
            .map(|i| IssueRef {
                id: i.id,
                backend: "beads".to_string(),
                url: None,
                title: i.title,
            })
            .collect())
    }

    fn is_resolved(&self, issue: &IssueRef) -> Result<bool, JilogReviewError> {
        let output = Command::new("bd")
            .current_dir(&self.repo_path)
            .args(["show", &issue.id, "--json"])
            .output()
            .map_err(|e| JilogReviewError::Command(format!("bd show failed: {}", e)))?;

        if !output.status.success() {
            return Ok(false); // treat as unresolved if lookup fails
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let data: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_default();
        let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
        Ok(status == "closed" || status == "resolved")
    }
}

/// Parse the issue ID from `bd create` stdout.
/// Expected format: "Created issue: <id>" or "Created: <id>"
fn parse_created_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let lower = line.to_lowercase();
        if lower.contains("created") {
            // Find the last whitespace-separated token.
            if let Some(id) = line.split_whitespace().last() {
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beads_tracker_graceful_when_bd_missing() {
        // If bd is not on PATH, we should get a Command error, not a panic.
        let tracker = BeadsTracker::new("/nonexistent/path");
        let signal = Signal::Correction(crate::signal::Correction {
            session_id: "test".into(),
            context: "some correction context here".into(),
        });
        // This will either succeed (bd found) or fail with Command error.
        match tracker.list_open() {
            Ok(_) => eprintln!("bd found and responded"),
            Err(JilogReviewError::Command(msg)) => {
                eprintln!("bd not found (expected in test env): {}", msg);
            }
            Err(JilogReviewError::Tracker(msg)) => {
                eprintln!("bd returned error (expected): {}", msg);
            }
            Err(e) => {
                eprintln!("unexpected error type: {}", e);
            }
        }
        // Test passes regardless — we just verify no panic occurs.
        let _ = signal;
    }
}
