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
}
