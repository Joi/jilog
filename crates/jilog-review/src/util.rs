//! Utility functions shared across jilog-review.
//!
//! Ported verbatim from opsctl/crates/opsctl/src/review_nightly.rs and
//! opsctl/crates/opsctl/src/config.rs.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// expand_tilde — port from opsctl/src/config.rs:155-161
// ---------------------------------------------------------------------------

/// Expand a leading `~/` to `$HOME/`. Passes through absolute and relative paths unchanged.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{}{}", home, &path[1..]));
        }
    }
    PathBuf::from(path)
}

/// Expand a leading `~/` in a GLOB PATTERN. Unlike [`expand_tilde`] the
/// expanded home prefix is escaped for `glob::glob`, so a home directory
/// containing glob metacharacters (`[`, `]`, `*`, `?`) matches literally
/// instead of being interpreted (or erroring) mid-pattern. The rest of
/// the pattern keeps its glob meaning.
pub fn expand_tilde_glob(pattern: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) => expand_tilde_glob_in(pattern, &home),
        Err(_) => pattern.to_string(),
    }
}

fn expand_tilde_glob_in(pattern: &str, home: &str) -> String {
    match pattern.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", glob::Pattern::escape(home), rest),
        None => pattern.to_string(),
    }
}

/// Contract a path under `$HOME` back to `~/...` for host-portable display
/// (the inverse of [`expand_tilde`], for paths shown in issue bodies).
pub fn contract_tilde(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Some(rest) = s.strip_prefix(&home) {
                if rest.starts_with('/') {
                    return format!("~{}", rest);
                }
            }
        }
    }
    s
}

/// The digest file a review run writes: `<digest_dir>/learning-digest-<date>.md`.
/// Single source of the filename formula so issue-body backlinks can never
/// drift from the actual filename (jilog#re4k).
pub fn digest_file_path(digest_dir: &std::path::Path, date_str: &str) -> PathBuf {
    digest_dir.join(format!("learning-digest-{}.md", date_str))
}

// ---------------------------------------------------------------------------
// truncate_chars — port from opsctl/src/review_nightly.rs:407-412
// ---------------------------------------------------------------------------

/// Char-aware truncation (not byte slicing — protects against UTF-8 panics).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

// ---------------------------------------------------------------------------
// truncate_with_marker — port from opsctl/src/review_nightly.rs:666-672
// ---------------------------------------------------------------------------

/// Truncate to `max` chars; append ` … [truncated]` suffix if truncated.
pub fn truncate_with_marker(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max).collect();
    format!("{} … [truncated]", truncated)
}

// ---------------------------------------------------------------------------
// python_repr — port from opsctl/src/review_nightly.rs:648-664
// ---------------------------------------------------------------------------

/// Approximate Python's `repr()` for a string: surround with single
/// quotes and escape backslashes / single quotes / newlines / tabs.
/// Not a perfect match for every Python repr edge case, but covers
/// the cases we hit in digest output.
pub fn python_repr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// parse_iso8601 — shared by the event-stream readers
// ---------------------------------------------------------------------------

/// Parse an ISO-8601 timestamp, with or without a timezone offset
/// (naive timestamps are taken as UTC). Returns None on failure so
/// callers can fall back or skip the line.
pub(crate) fn parse_iso8601(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    None
}

// ---------------------------------------------------------------------------
// json_decimal — shared by the usage/spend readers
// ---------------------------------------------------------------------------

/// Read a JSON value as a [`rust_decimal::Decimal`].
///
/// Numbers go through their shortest-roundtrip text (what `serde_json`
/// prints), which reproduces the upstream literal for any realistic cost
/// value; strings are parsed verbatim. Null, missing, and unparseable
/// values are treated as "no cost".
pub(crate) fn json_decimal(v: &serde_json::Value) -> Option<rust_decimal::Decimal> {
    use std::str::FromStr;
    match v {
        serde_json::Value::Number(n) => rust_decimal::Decimal::from_str(&n.to_string()).ok(),
        serde_json::Value::String(s) => rust_decimal::Decimal::from_str(s).ok(),
        _ => None,
    }
}

/// Run `cmd` with stdin closed and both pipes captured, bounded by
/// `timeout` end to end: the child runs in its own process group (unix) so
/// a descendant that inherited a pipe dies with it, the exit wait and the
/// pipe drain share one deadline, and on expiry the whole group is killed.
/// A daemon mid-sync makes `agentsview usage daily` block for minutes
/// (observed 2026-09-16); the nightly must never wait on it (jilog#heyg).
pub fn run_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, crate::error::JilogReviewError> {
    use crate::error::JilogReviewError;
    use std::io::Read as _;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Instant;

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| JilogReviewError::Reader("timeout too large".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    #[cfg(unix)]
    let pgid = child.id();
    let kill_group = |child: &mut std::process::Child| {
        #[cfg(unix)]
        // SAFETY: plain libc call; a negative pid addresses the process
        // group created by process_group(0), whose id is the child's pid.
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let tx_out = tx.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx_out.send((0, buf));
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = tx.send((1, buf));
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            return Err(JilogReviewError::Reader(format!(
                "command timed out after {:?}",
                timeout
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    // Drain both pipes under the same deadline: a descendant that inherited
    // a pipe keeps it open after the direct child exits.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok((0, buf)) => out = buf,
            Ok((_, buf)) => err = buf,
            Err(_) => {
                kill_group(&mut child);
                return Err(JilogReviewError::Reader(format!(
                    "command timed out after {:?} waiting for output (a descendant kept the pipe open)",
                    timeout
                )));
            }
        }
    }
    Ok(std::process::Output { status, stdout: out, stderr: err })
}

#[cfg(all(test, unix))]
mod timeout_tests {
    use super::run_with_timeout;

    #[test]
    fn run_with_timeout_returns_inside_the_deadline_when_a_descendant_holds_the_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("orphan.sh");
        // The direct child exits at once; its background grandchild keeps
        // stdout open for 30s. Without a group kill + bounded drain this
        // would block on the pipe until the grandchild exits.
        std::fs::write(&script, "#!/bin/sh\n( sleep 30 ) &\necho started\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let err = run_with_timeout(
            &mut std::process::Command::new(&script),
            std::time::Duration::from_millis(500),
        )
        .unwrap_err()
        .to_string();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "returned in {:?}",
            started.elapsed()
        );
        assert!(err.contains("timed out"), "{err}");
        // A well-behaved child returns its output and status.
        let ok = dir.path().join("ok.sh");
        std::fs::write(&ok, "#!/bin/sh\necho out\necho err >&2\nexit 0\n").unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = run_with_timeout(
            &mut std::process::Command::new(&ok),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from opsctl/src/review_nightly.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_handles_unicode() {
        let s = "日本語";
        // 3 chars, should not truncate
        assert_eq!(truncate_chars(s, 3), "日本語");
        assert_eq!(truncate_chars(s, 2), "日本");
    }

    #[test]
    fn python_repr_basic_quoting() {
        assert_eq!(python_repr("hello"), "'hello'");
        assert_eq!(python_repr("it's"), "'it\\'s'");
        assert_eq!(python_repr("a\nb"), "'a\\nb'");
    }

    #[test]
    fn expand_tilde_basic() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            let expanded = expand_tilde("~/foo/bar");
            assert_eq!(expanded, PathBuf::from(format!("{}/foo/bar", home)));
        }
    }

    #[test]
    fn expand_tilde_no_tilde() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn expand_tilde_glob_escapes_metacharacters_in_home() {
        // A home dir with glob metacharacters must match itself literally
        // while the pattern remainder keeps its glob meaning.
        let expanded = expand_tilde_glob_in("~/x/*.jsonl", "/Users/we[i]rd*");
        assert_eq!(
            expanded,
            format!("{}/x/*.jsonl", glob::Pattern::escape("/Users/we[i]rd*"))
        );
        let pat = glob::Pattern::new(&expanded).unwrap();
        assert!(pat.matches("/Users/we[i]rd*/x/session.jsonl"));
        assert!(!pat.matches("/Users/weird/x/session.jsonl"));
        // Non-tilde patterns pass through untouched.
        assert_eq!(expand_tilde_glob_in("/var/log/*.jsonl", "/home/x"), "/var/log/*.jsonl");
    }

    #[test]
    fn truncate_with_marker_appends_suffix() {
        let s = "x".repeat(10);
        let result = truncate_with_marker(&s, 5);
        assert!(result.contains("[truncated]"));
        assert!(result.starts_with("xxxxx"));
    }

    #[test]
    fn truncate_with_marker_no_truncation() {
        let result = truncate_with_marker("short", 100);
        assert_eq!(result, "short");
        assert!(!result.contains("[truncated]"));
    }

    #[test]
    fn contract_tilde_home_prefix_and_passthrough() {
        let home = std::env::var("HOME").expect("HOME set in tests");
        let under = std::path::PathBuf::from(&home).join("x/y.md");
        assert_eq!(contract_tilde(&under), "~/x/y.md");
        // Exactly HOME (no trailing segment) stays as-is — no bare "~".
        assert_eq!(contract_tilde(std::path::Path::new(&home)), home);
        // Prefix that merely STARTS with the home string must not contract.
        let sibling = format!("{}2/x.md", home);
        assert_eq!(contract_tilde(std::path::Path::new(&sibling)), sibling);
        // Outside HOME passes through.
        assert_eq!(contract_tilde(std::path::Path::new("/var/log/x.md")), "/var/log/x.md");
    }

    #[test]
    fn digest_file_path_formula() {
        let p = digest_file_path(std::path::Path::new("/tmp/d"), "2026-08-26");
        assert_eq!(p, std::path::PathBuf::from("/tmp/d/learning-digest-2026-08-26.md"));
    }
}
