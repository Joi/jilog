//! Signal detectors — heuristic analysis of transcript messages.
//!
//! All four detectors are pure functions over `&[Message]`.
//! Ported verbatim from opsctl/crates/opsctl/src/review_nightly.rs.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::RegexSet;

use crate::reader::Message;
use crate::signal::{Correction, ErrorSignal, Workaround};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum trimmed length for a user message to count as a "correction"
/// candidate. Matches `MIN_CORRECTION_LENGTH` in the Python script.
const MIN_CORRECTION_LENGTH: usize = 15;

/// Maximum (raw, untrimmed) length of a user message. Anything longer
/// is treated as a fresh prompt, not a correction.
const MAX_CORRECTION_LENGTH: usize = 200;

/// Maximum digest output for an error message (truncated tail gets
/// the " … [truncated]" suffix). Matches `MAX_ERROR_MESSAGE_LENGTH`.
pub(crate) const MAX_ERROR_MESSAGE_LENGTH: usize = 500;

/// Workaround pattern regexes — at most ONE workaround per message; first-match wins.
const WORKAROUND_PATTERNS: &[&str] = &[
    r"(?i)for now",
    r"(?i)temporary",
    r"(?i)workaround",
    r"(?i)hardcoded",
    r"(?i)TODO",
    r"(?i)FIXME",
    r"(?i)quick fix",
    r"(?i)hack",
];

/// Human-readable labels (parallel to WORKAROUND_PATTERNS by index).
const WORKAROUND_LABELS: &[&str] = &[
    "for now",
    "temporary",
    "workaround",
    "hardcoded",
    "TODO",
    "FIXME",
    "quick fix",
    "hack",
];

/// Sub-agent session prefix (16 zeros). Sessions whose ID starts with
/// this are excluded from P0 alert counting.
const SUB_AGENT_PREFIX: &str = "0000000000000000";

/// Threshold for the P0 alert: distinct root sessions per tool.
const P0_DISTINCT_SESSION_THRESHOLD: usize = 3;

// ---------------------------------------------------------------------------
// Heuristic 1: Correction detection
// ---------------------------------------------------------------------------

/// Detect "correction" patterns: an assistant→user→assistant triple
/// where the user message is short (15..=200 chars).
pub fn detect_corrections(messages: &[Message], session_id: &str) -> Vec<Correction> {
    if messages.len() < 3 {
        return Vec::new();
    }

    let mut out = Vec::new();
    for window in messages.windows(3) {
        let a = &window[0];
        let u = &window[1];
        let b = &window[2];

        if a.role.as_deref() != Some("assistant") {
            continue;
        }
        if u.role.as_deref() != Some("user") {
            continue;
        }
        if b.role.as_deref() != Some("assistant") {
            continue;
        }

        let content_str = content_to_string(&u.content);
        if content_str.len() > MAX_CORRECTION_LENGTH {
            continue;
        }
        if content_str.trim().len() < MIN_CORRECTION_LENGTH {
            continue;
        }

        out.push(Correction {
            session_id: session_id.to_string(),
            context: content_str,
        });
    }
    out
}

/// Mirror of Python's `str(value)` coercion for the corrections heuristic.
fn content_to_string(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Heuristic 2: Error signal detection
// ---------------------------------------------------------------------------

/// Scan for `role: tool` messages whose `content` parses as JSON with
/// `success: false`. Returns one ErrorSignal per matching message.
pub fn detect_errors(messages: &[Message], session_id: &str) -> Vec<ErrorSignal> {
    let mut out = Vec::new();
    for msg in messages {
        if msg.role.as_deref() != Some("tool") {
            continue;
        }
        let content_str = match &msg.content {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => continue,
        };

        let data: serde_json::Value = match serde_json::from_str(&content_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Strict: must be exactly false (matches Python `data.get("success") is False`).
        if data.get("success") != Some(&serde_json::Value::Bool(false)) {
            continue;
        }

        let tool_name = msg.name.clone().unwrap_or_else(|| "unknown".to_string());
        let message = extract_error_message(&data);

        out.push(ErrorSignal {
            session_id: session_id.to_string(),
            tool_name,
            message,
        });
    }
    out
}

/// Pull the most useful error string out of a tool result `data` blob.
/// Precedence (matches Python):
/// 1. `error[0]` if `error` is a non-empty list
/// 2. `error` as scalar string (or any scalar, str-coerced)
/// 3. The whole `data` blob serialized as fallback
fn extract_error_message(data: &serde_json::Value) -> String {
    if let Some(err) = data.get("error") {
        match err {
            serde_json::Value::Array(arr) if !arr.is_empty() => {
                return value_as_string(&arr[0]);
            }
            serde_json::Value::Array(_) => {
                // Empty list: fall through to whole-data fallback.
            }
            serde_json::Value::Null => {
                // null is treated as "absent" — fall through.
            }
            other => return value_as_string(other),
        }
    }
    // Fallback: serialize whole data blob.
    data.to_string()
}

fn value_as_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Heuristic 3: Workaround detection
// ---------------------------------------------------------------------------

/// Compile the workaround regex set once.
fn workaround_regex() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        RegexSet::new(WORKAROUND_PATTERNS).expect("workaround patterns must compile")
    })
}

/// Detect workaround language in assistant messages. At most one
/// workaround per message (first-match wins, in declaration order).
pub fn detect_workarounds(messages: &[Message], session_id: &str) -> Vec<Workaround> {
    let regex = workaround_regex();
    let mut out = Vec::new();
    for msg in messages {
        if msg.role.as_deref() != Some("assistant") {
            continue;
        }
        let text = extract_assistant_text(&msg.content);
        if text.is_empty() {
            continue;
        }
        let matches = regex.matches(&text);
        if !matches.matched_any() {
            continue;
        }
        // Find the FIRST matched pattern in declaration order.
        let first_idx = matches.iter().next().expect("matched_any is true");
        let label = WORKAROUND_LABELS
            .get(first_idx)
            .copied()
            .unwrap_or("unknown");

        out.push(Workaround {
            session_id: session_id.to_string(),
            pattern: label.to_string(),
            context: crate::util::truncate_chars(&text, 200),
        });
    }
    out
}

/// Extract assistant text from a content value, EXCLUDING tool_use /
/// tool_result blocks.
pub(crate) fn extract_assistant_text(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for block in arr {
                match block {
                    serde_json::Value::String(s) => parts.push(s.clone()),
                    serde_json::Value::Object(map) => {
                        if map.get("type") == Some(&serde_json::Value::String("text".into())) {
                            if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                                parts.push(text.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            parts.join("\n")
        }
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Heuristic 4: P0 alert detection
// ---------------------------------------------------------------------------

/// Group errors by tool, count distinct ROOT sessions (skip sub-agents).
/// Returns entries with >= P0_DISTINCT_SESSION_THRESHOLD distinct sessions.
pub fn detect_p0_alerts(errors: &[ErrorSignal]) -> HashMap<String, BTreeSet<String>> {
    let mut by_tool: HashMap<String, BTreeSet<String>> = HashMap::new();
    for e in errors {
        if e.session_id.starts_with(SUB_AGENT_PREFIX) {
            continue;
        }
        by_tool
            .entry(e.tool_name.clone())
            .or_default()
            .insert(e.session_id.clone());
    }
    by_tool.retain(|_, sessions| sessions.len() >= P0_DISTINCT_SESSION_THRESHOLD);
    by_tool
}

// ---------------------------------------------------------------------------
// Tests — ported from opsctl/crates/opsctl/src/review_nightly.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(text: &str) -> Message {
        Message {
            role: Some("assistant".into()),
            content: Some(json!(text)),
            name: None,
        }
    }

    fn user(text: &str) -> Message {
        Message {
            role: Some("user".into()),
            content: Some(json!(text)),
            name: None,
        }
    }

    fn tool(name: &str, content: serde_json::Value) -> Message {
        Message {
            role: Some("tool".into()),
            content: Some(json!(content.to_string())),
            name: Some(name.into()),
        }
    }

    // ---------- corrections ----------

    #[test]
    fn corrections_basic_triple() {
        let msgs = vec![
            assistant("first reply"),
            user("no, you misunderstood the goal here"),
            assistant("second reply"),
        ];
        let out = detect_corrections(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "s1");
        assert_eq!(out[0].context, "no, you misunderstood the goal here");
    }

    #[test]
    fn corrections_too_short_skipped() {
        // 12 chars trimmed -- below MIN
        let msgs = vec![
            assistant("first"),
            user("just a short"),
            assistant("second"),
        ];
        assert!(detect_corrections(&msgs, "s1").is_empty());
    }

    #[test]
    fn corrections_too_long_skipped() {
        let long = "x".repeat(201);
        let msgs = vec![assistant("a"), user(&long), assistant("b")];
        assert!(detect_corrections(&msgs, "s1").is_empty());
    }

    #[test]
    fn corrections_wrong_role_pattern_skipped() {
        let msgs = vec![
            user("first message in transcript"),
            user("another short user message"),
            assistant("reply"),
        ];
        assert!(detect_corrections(&msgs, "s1").is_empty());
    }

    #[test]
    fn corrections_multiple_in_one_transcript() {
        let msgs = vec![
            assistant("a1"),
            user("first correction please"),
            assistant("a2"),
            user("second correction please"),
            assistant("a3"),
        ];
        let out = detect_corrections(&msgs, "s1");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn corrections_empty_transcript() {
        assert!(detect_corrections(&[], "s1").is_empty());
        assert!(detect_corrections(&[assistant("a")], "s1").is_empty());
        assert!(detect_corrections(&[assistant("a"), user("hi there friend")], "s1").is_empty());
    }

    // ---------- errors ----------

    #[test]
    fn errors_success_false_detected() {
        let msgs = vec![tool("bash", json!({"success": false, "error": "boom"}))];
        let out = detect_errors(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tool_name, "bash");
        assert_eq!(out[0].message, "boom");
    }

    #[test]
    fn errors_success_true_skipped() {
        let msgs = vec![tool("bash", json!({"success": true, "output": "ok"}))];
        assert!(detect_errors(&msgs, "s1").is_empty());
    }

    #[test]
    fn errors_not_tool_role_skipped() {
        let msgs = vec![Message {
            role: Some("user".into()),
            content: Some(json!(json!({"success": false}).to_string())),
            name: None,
        }];
        assert!(detect_errors(&msgs, "s1").is_empty());
    }

    #[test]
    fn errors_invalid_json_content_skipped() {
        let msgs = vec![Message {
            role: Some("tool".into()),
            content: Some(json!("not valid json {")),
            name: Some("bash".into()),
        }];
        assert!(detect_errors(&msgs, "s1").is_empty());
    }

    #[test]
    fn errors_error_as_list_takes_first() {
        let msgs = vec![tool(
            "validator",
            json!({"success": false, "error": ["first issue", "second issue"]}),
        )];
        let out = detect_errors(&msgs, "s1");
        assert_eq!(out[0].message, "first issue");
    }

    #[test]
    fn errors_error_null_falls_back_to_data() {
        let msgs = vec![tool(
            "weird",
            json!({"success": false, "error": null, "info": "see logs"}),
        )];
        let out = detect_errors(&msgs, "s1");
        assert!(out[0].message.contains("see logs"));
    }

    #[test]
    fn errors_no_tool_name_falls_back() {
        let msg = Message {
            role: Some("tool".into()),
            content: Some(json!(json!({"success": false, "error": "x"}).to_string())),
            name: None,
        };
        let out = detect_errors(&[msg], "s1");
        assert_eq!(out[0].tool_name, "unknown");
    }

    // ---------- workarounds ----------

    #[test]
    fn workarounds_basic_for_now() {
        let msgs = vec![assistant("Using a hardcoded value for now until config is wired.")];
        let out = detect_workarounds(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pattern, "for now");
    }

    #[test]
    fn workarounds_one_per_message() {
        let msgs = vec![assistant("TODO: refactor this hack workaround later.")];
        let out = detect_workarounds(&msgs, "s1");
        assert_eq!(out.len(), 1, "at most one workaround per message");
    }

    #[test]
    fn workarounds_skip_tool_use_blocks() {
        let msgs = vec![Message {
            role: Some("assistant".into()),
            content: Some(json!([
                {"type": "tool_use", "name": "bash", "input": {"todo_field": "TODO"}},
                {"type": "text", "text": "I ran the command."},
            ])),
            name: None,
        }];
        let out = detect_workarounds(&msgs, "s1");
        assert!(out.is_empty(), "tool_use blocks must be skipped");
    }

    #[test]
    fn workarounds_text_blocks_in_list() {
        let msgs = vec![Message {
            role: Some("assistant".into()),
            content: Some(json!([
                {"type": "text", "text": "Quick fix incoming."},
            ])),
            name: None,
        }];
        let out = detect_workarounds(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pattern, "quick fix");
    }

    #[test]
    fn workarounds_case_insensitive() {
        let msgs = vec![assistant("HACK: this is gross.")];
        let out = detect_workarounds(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pattern, "hack");
    }

    #[test]
    fn workarounds_no_match_clean_text() {
        let msgs = vec![assistant("All systems nominal. Tests passing.")];
        assert!(detect_workarounds(&msgs, "s1").is_empty());
    }

    #[test]
    fn workarounds_user_role_skipped() {
        let msgs = vec![user("TODO this is from the user, should not match")];
        assert!(detect_workarounds(&msgs, "s1").is_empty());
    }

    #[test]
    fn workarounds_context_truncated_to_200() {
        let long_text = format!("FIXME {}", "x".repeat(300));
        let msgs = vec![assistant(&long_text)];
        let out = detect_workarounds(&msgs, "s1");
        assert_eq!(out[0].context.chars().count(), 200);
    }

    // ---------- p0 alerts ----------

    #[test]
    fn p0_alerts_threshold_three_distinct() {
        let errors = vec![
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "x".into() },
            ErrorSignal { session_id: "b".into(), tool_name: "bash".into(), message: "y".into() },
            ErrorSignal { session_id: "c".into(), tool_name: "bash".into(), message: "z".into() },
        ];
        let p0 = detect_p0_alerts(&errors);
        assert_eq!(p0.len(), 1);
        assert_eq!(p0["bash"].len(), 3);
    }

    #[test]
    fn p0_alerts_two_distinct_skipped() {
        let errors = vec![
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "x".into() },
            ErrorSignal { session_id: "b".into(), tool_name: "bash".into(), message: "y".into() },
        ];
        assert!(detect_p0_alerts(&errors).is_empty());
    }

    #[test]
    fn p0_alerts_same_session_repeated_doesnt_count_twice() {
        let errors = vec![
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "1".into() },
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "2".into() },
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "3".into() },
        ];
        // Only 1 distinct session → below threshold
        assert!(detect_p0_alerts(&errors).is_empty());
    }

    #[test]
    fn p0_alerts_subagent_sessions_excluded() {
        let errors = vec![
            ErrorSignal { session_id: "00000000000000001".into(), tool_name: "bash".into(), message: "x".into() },
            ErrorSignal { session_id: "00000000000000002".into(), tool_name: "bash".into(), message: "y".into() },
            ErrorSignal { session_id: "00000000000000003".into(), tool_name: "bash".into(), message: "z".into() },
        ];
        assert!(
            detect_p0_alerts(&errors).is_empty(),
            "sub-agent sessions (16 leading zeros) must not count toward P0"
        );
    }

    // ---------- extract_assistant_text ----------

    #[test]
    fn extract_assistant_text_skips_non_text_blocks() {
        let content = json!([
            {"type": "tool_use", "name": "x", "input": {"foo": "bar"}},
            {"type": "text", "text": "hello"},
            {"type": "tool_result", "content": "result body"},
            {"type": "text", "text": "world"},
        ]);
        let out = extract_assistant_text(&Some(content));
        assert_eq!(out, "hello\nworld");
    }
}
