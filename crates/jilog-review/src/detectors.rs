//! Signal detectors — heuristic analysis of transcript messages.
//!
//! All five detectors are pure functions over `&[Message]`.
//! Ported verbatim from opsctl/crates/opsctl/src/review_nightly.rs.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::RegexSet;

use crate::reader::Message;
use crate::signal::{Correction, DeferralSignal, ErrorSignal, Workaround};

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

/// Deferral pattern regexes — at most ONE deferral per message; first-match wins.
const DEFERRAL_PATTERNS: &[&str] = &[
    r"(?i)\bI'?ll come back to (this|that|it)",
    r"(?i)\bdeferr?ing (this|that|it|until)",
    r"(?i)\bdefer (this|that|it)(?: (to|until|for))?",
    r"(?i)\bpunt(ing)? on (this|that|it)",
    r"(?i)\bleav(e|ing) (this|that|it) for (later|now|next)",
    r"(?i)\bskipping for now",
    r"(?i)\bpark(ing)? (this|that|it) for now",
    r"(?i)\bnext session",
    r"(?i)\bcircle back (to|on)",
];

/// Human-readable labels (parallel to DEFERRAL_PATTERNS by index).
const DEFERRAL_LABELS: &[&str] = &[
    "come back later",
    "deferring",
    "defer",
    "punt",
    "leave for later",
    "skipping for now",
    "park for now",
    "next session",
    "circle back",
];

/// Threshold for the P0 alert: distinct root sessions per tool.
const P0_DISTINCT_SESSION_THRESHOLD: usize = 3;

/// Corrective-marker regexes for the chat-tuned correction detector.
///
/// In a group chat, a short user message after an assistant turn is usually
/// just conversation (often not even addressed to the bot), so the coding
/// heuristic's length window alone would flag most of the channel. Chat
/// sessions additionally require explicit corrective language.
const CHAT_CORRECTION_PATTERNS: &[&str] = &[
    r"(?i)^no[,.! ]",
    r"(?i)\bdon'?t\b",
    r"(?i)\bdo not\b",
    r"(?i)\bplease stop\b",
    r"(?i)\bstop (doing|replying|posting|answering|sending|using|adding)\b",
    r"(?i)\bwrong\b",
    r"(?i)\bincorrect\b",
    r"(?i)\bnot (that|what|like that|right|the right)\b",
    r"(?i)\bshould(n'?t| not| never)\b",
    r"(?i)\bthat'?s not\b",
];

// ---------------------------------------------------------------------------
// Heuristic 1: Correction detection
// ---------------------------------------------------------------------------

/// Detect "correction" patterns: an assistant→user→assistant triple
/// where the user message is short (15..=200 chars).
pub fn detect_corrections(messages: &[Message], session_id: &str) -> Vec<Correction> {
    detect_corrections_impl(messages, session_id, false)
}

/// Chat-tuned variant of [`detect_corrections`] for fleet/chat sessions
/// (transcript handles with a persona): same triple + length window, but the
/// user message must also match a corrective-language pattern
/// ([`CHAT_CORRECTION_PATTERNS`]).
pub fn detect_corrections_chat(messages: &[Message], session_id: &str) -> Vec<Correction> {
    detect_corrections_impl(messages, session_id, true)
}

fn detect_corrections_impl(messages: &[Message], session_id: &str, chat: bool) -> Vec<Correction> {
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

        // A "user" turn whose content is a tool_result block is a tool echo
        // injected by the runtime (Amplifier/Claude represent tool results as
        // user-role messages), NOT a genuine user correction. Exclude these
        // regardless of `is_error`. See jilog#21yg.
        if contains_tool_result(&u.content) {
            continue;
        }

        let content_str = content_to_string(&u.content);
        if content_str.len() > MAX_CORRECTION_LENGTH {
            continue;
        }
        if content_str.trim().len() < MIN_CORRECTION_LENGTH {
            continue;
        }
        // Trimmed so the `^`-anchored markers see the real first word.
        if chat && !chat_correction_regex().is_match(content_str.trim()) {
            continue;
        }

        out.push(Correction {
            session_id: session_id.to_string(),
            context: content_str,
            ..Default::default()
        });
    }
    out
}

/// Compile the chat corrective-marker regex set once.
fn chat_correction_regex() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        RegexSet::new(CHAT_CORRECTION_PATTERNS).expect("chat correction patterns must compile")
    })
}

/// Mirror of Python's `str(value)` coercion for the corrections heuristic.
fn content_to_string(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// True if the message content is an array containing at least one
/// `tool_result` block. Such "user" turns are tool echoes injected by the
/// runtime, not genuine user corrections. See jilog#21yg.
fn contains_tool_result(content: &Option<serde_json::Value>) -> bool {
    match content {
        Some(serde_json::Value::Array(arr)) => arr.iter().any(|block| {
            block.get("type") == Some(&serde_json::Value::String("tool_result".into()))
        }),
        _ => false,
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
        if is_expected_noise(&tool_name, &data) {
            tracing::debug!(
                "detect_errors: suppressed expected-noise result from `{}` in {} (jilog#42fd)",
                tool_name, session_id
            );
            continue;
        }
        let message = extract_error_message(&data);

        out.push(ErrorSignal {
            session_id: session_id.to_string(),
            tool_name,
            message,
            ..Default::default()
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
// Expected-noise filter (jilog#42fd)
// ---------------------------------------------------------------------------
//
// Joi's 2026-09-01 ruling: a denied mode-switch prompt and a bare nonzero
// exit code are not error signals. The filter is an ALLOWLIST of positively
// identified noise shapes — never a denylist of "non-diagnostic" text. Any
// result that does not match one of the shapes exactly (a non-string stdout,
// a structured error object, an unfamiliar envelope) is emitted, so real
// error detection is never weakened by a heuristic guess.

/// True when a `success: false` result is one of the ruled-expected shapes
/// and must not become an [`ErrorSignal`].
fn is_expected_noise(tool_name: &str, data: &serde_json::Value) -> bool {
    is_mode_denial(tool_name, data) || is_content_free_bash_failure(tool_name, data)
}

/// The `mode` tool refusing a switch/clear: `output.status == "denied"`, an
/// `output.denied_mode` field, or an error code ending in `_denied`
/// (`clear_denied`, `switch_denied`). A `mode` error that is not a denial
/// (bad transition, internal failure) does not match and is emitted.
fn is_mode_denial(tool_name: &str, data: &serde_json::Value) -> bool {
    if tool_name != "mode" {
        return false;
    }
    let output = data.get("output");
    let status_denied = output
        .and_then(|o| o.get("status"))
        .and_then(|s| s.as_str())
        == Some("denied");
    // Presence of the field is the marker, whatever its value (a null
    // denied_mode is still the tool reporting a denial).
    let has_denied_mode = output.and_then(|o| o.get("denied_mode")).is_some();
    let code_denied = [
        data.get("error").and_then(|e| e.get("code")),
        data.get("code"),
    ]
    .into_iter()
    .flatten()
    .filter_map(|c| c.as_str())
    .any(|c| c.ends_with("_denied"));
    status_denied || has_denied_mode || code_denied
}

/// Regex for the bare timeout sentence the bash tool emits.
fn bare_timeout_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)^command timed out after \d+ seconds?\.?$")
            .expect("bare timeout regex must compile")
    })
}

/// The error text of a failed result, classified.
#[derive(Debug, PartialEq, Eq)]
enum ErrText {
    /// `error` null/absent and no top-level `message`.
    Absent,
    /// A string at `error`, `error.message`, or top-level `message`.
    Text(String),
    /// An unrecognized shape; never suppressed. Either `error` is present
    /// but not a string and not an object with a string `message`, or a
    /// top-level `message` is not the text that was read (present and
    /// non-null beside a non-null `error`, or present and non-string with
    /// `error` absent).
    Unrecognized,
}

fn error_text(data: &serde_json::Value) -> ErrText {
    // A top-level `message` is only ever read when `error` is null/absent.
    // Beside a non-null `error` it would be an unread field that could carry
    // a diagnostic, so that combination is unrecognized (Stage 1 round 2).
    // A null `message` is absent, like every other null in this filter
    // (roborev #1865: serializers that emit all keys).
    let has_message = data.get("message").is_some_and(|v| !v.is_null());
    match data.get("error") {
        None | Some(serde_json::Value::Null) => match data.get("message") {
            Some(serde_json::Value::String(s)) => ErrText::Text(s.clone()),
            Some(_) => ErrText::Unrecognized,
            None => ErrText::Absent,
        },
        Some(_) if has_message => ErrText::Unrecognized,
        Some(serde_json::Value::String(s)) => ErrText::Text(s.clone()),
        Some(serde_json::Value::Object(map)) => match map.get("message") {
            Some(serde_json::Value::String(s)) => ErrText::Text(s.clone()),
            _ => ErrText::Unrecognized,
        },
        Some(_) => ErrText::Unrecognized,
    }
}

/// `output.<key>` as text: `Some("")` when absent or null, `Some(s)` for a
/// string, `None` for any other type — a structured stdout/stderr is an
/// unrecognized shape and the caller must emit.
fn output_text(output: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    match output.get(key) {
        None | Some(serde_json::Value::Null) => Some(String::new()),
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

/// The only keys a recognized bash envelope may carry. Anything else —
/// `output.message: "disk full"`, a sibling beside the timeout's
/// `error.message`, an extra top-level field — is content the shape does
/// not understand, so the shape does not match and the caller emits
/// (fresheyes 2026-09-03 pass 1).
const BARE_TOP_KEYS: &[&str] = &["success", "error", "output", "message"];
const BARE_OUTPUT_KEYS: &[&str] = &["returncode", "stdout", "stderr"];
const BARE_ERROR_KEYS: &[&str] = &["message"];

fn keys_within(map: &serde_json::Map<String, serde_json::Value>, allowed: &[&str]) -> bool {
    map.keys().all(|k| allowed.contains(&k.as_str()))
}

/// True only when the envelope carries nothing but the fields the two bare
/// shapes are defined over: top-level keys within [`BARE_TOP_KEYS`], `error`
/// null/string/or an object with only `message`, and `output` absent/null,
/// a string, or an object with only [`BARE_OUTPUT_KEYS`]. An array or
/// number `output`, or any unlisted key, makes the envelope unrecognized
/// (roborev #1858). A string `output` is allowed here because the real
/// Amplifier bash timeout envelope is
/// `{"error":{"message":"Command timed out after 30 seconds"},"output":
/// "Command timed out after 30 seconds","success":false}` (tool_bash sets
/// `output = error_msg`); [`output_is_blank`] decides whether that string
/// carries anything.
fn envelope_is_bare(data: &serde_json::Value) -> bool {
    let Some(top) = data.as_object() else { return false };
    if !keys_within(top, BARE_TOP_KEYS) {
        return false;
    }
    let error_ok = match top.get("error") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Object(e)) => keys_within(e, BARE_ERROR_KEYS),
        Some(_) => false,
    };
    let output_ok = match top.get("output") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Object(o)) => keys_within(o, BARE_OUTPUT_KEYS),
        Some(_) => false,
    };
    error_ok && output_ok
}

/// True only when `output` carries nothing: absent/null; an object whose
/// `stdout` and `stderr` are both absent, null, or whitespace-only strings
/// and whose `returncode`, if present, is an integer (null included in
/// "not an integer"); or a string that is empty or is itself the bare
/// timeout sentence (the
/// production timeout envelope echoes the sentence into `output` — Stage 1
/// review, 5/5 transcript samples). A string with any other text (a partial
/// log, a traceback) is content and is never blank; a non-string stream or
/// a non-object, non-string `output` is never blank either.
fn output_is_blank(data: &serde_json::Value) -> bool {
    match data.get("output") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            t.is_empty() || bare_timeout_regex().is_match(t)
        }
        Some(serde_json::Value::Object(output)) => {
            // `returncode` is the only other allowed key; when present it
            // must be an integer (null included — a present non-integer is
            // a shape the filter does not read), else the output is not
            // blank (fresheyes round 3).
            let returncode_ok = match output.get("returncode") {
                None => true,
                Some(v) => v.as_i64().is_some(),
            };
            returncode_ok
                && matches!(
                    (output_text(output, "stdout"), output_text(output, "stderr")),
                    (Some(out), Some(err)) if out.trim().is_empty() && err.trim().is_empty()
                )
        }
        Some(_) => false,
    }
}

/// Shape 1: the timeout sentence with nothing else — no stdout, no stderr,
/// no other field anywhere in the envelope.
fn is_bare_timeout(data: &serde_json::Value) -> bool {
    envelope_is_bare(data)
        && match error_text(data) {
            ErrText::Text(t) => bare_timeout_regex().is_match(t.trim()) && output_is_blank(data),
            _ => false,
        }
}

/// Shape 2: an integer nonzero `output.returncode`, no error text at all,
/// blank stdout and stderr, and no other field anywhere in the envelope. A
/// banner-only stdout does NOT match: the only way to call it non-diagnostic
/// would be a marker heuristic.
fn is_bare_nonzero_exit(data: &serde_json::Value) -> bool {
    let nonzero = data
        .get("output")
        .and_then(|o| o.get("returncode"))
        .and_then(|r| r.as_i64())
        .map(|r| r != 0)
        .unwrap_or(false);
    envelope_is_bare(data)
        && nonzero
        && error_text(data) == ErrText::Absent
        && output_is_blank(data)
}

/// The two content-free bash shapes (bare timeout, bare nonzero exit).
fn is_content_free_bash_failure(tool_name: &str, data: &serde_json::Value) -> bool {
    tool_name == "bash" && (is_bare_timeout(data) || is_bare_nonzero_exit(data))
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
            ..Default::default()
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
// Heuristic 4: Deferral detection
// ---------------------------------------------------------------------------

/// Compile the deferral regex set once.
fn deferral_regex() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| RegexSet::new(DEFERRAL_PATTERNS).expect("deferral patterns must compile"))
}

/// Detect deferral language in assistant messages. At most one
/// deferral per message (first-match wins, in declaration order).
pub fn detect_deferrals(messages: &[Message], session_id: &str) -> Vec<DeferralSignal> {
    let regex = deferral_regex();
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
        let label = DEFERRAL_LABELS.get(first_idx).copied().unwrap_or("unknown");

        out.push(DeferralSignal {
            session_id: session_id.to_string(),
            item: label.to_string(),
            ..Default::default()
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Heuristic 5: P0 alert detection
// ---------------------------------------------------------------------------

/// Group errors by tool, count distinct ROOT sessions (skip sub-agents).
/// Returns entries with >= P0_DISTINCT_SESSION_THRESHOLD distinct sessions.
pub fn detect_p0_alerts(errors: &[ErrorSignal]) -> HashMap<String, BTreeSet<String>> {
    let mut by_tool: HashMap<String, BTreeSet<String>> = HashMap::new();
    for e in errors {
        if crate::reader::is_sub_agent_session(&e.session_id) {
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

    /// Helper: a user-role turn carrying a single tool_result content block,
    /// mirroring how Amplifier/Claude transcripts echo tool output.
    fn tool_result_user(content: &str, is_error: bool) -> Message {
        Message {
            role: Some("user".into()),
            content: Some(json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_x",
                    "is_error": is_error,
                    "content": content,
                }
            ])),
            name: None,
        }
    }

    #[test]
    fn corrections_tool_result_user_turn_excluded() {
        // Regression for jilog#21yg: tool_result echoes (is_error=false) must
        // NOT be classified as corrections.
        let msgs = vec![
            assistant("ran it"),
            tool_result_user("(Bash completed with no output)", false),
            assistant("next"),
        ];
        assert!(
            detect_corrections(&msgs, "s1").is_empty(),
            "tool_result user turns must not be corrections"
        );
    }

    #[test]
    fn corrections_tool_result_error_user_turn_excluded() {
        // jilog#21yg: even is_error=true tool_result blocks are tool echoes,
        // not user corrections.
        let msgs = vec![
            assistant("a"),
            tool_result_user(
                "<tool_use_error>File has not been read yet. Read it first.</tool_use_error>",
                true,
            ),
            assistant("b"),
        ];
        assert!(detect_corrections(&msgs, "s1").is_empty());
    }

    #[test]
    fn corrections_real_user_string_still_detected() {
        // Genuine short user corrections (plain string content) must survive
        // the tool_result filter unchanged.
        let msgs = vec![
            assistant("first"),
            user("no, you misunderstood the goal here"),
            assistant("second"),
        ];
        let out = detect_corrections(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].context, "no, you misunderstood the goal here");
    }

    #[test]
    fn corrections_digest_2026_06_24_fixture() {
        // Fixture drawn from the 2026-06-24 learning digest (jilog#21yg): a
        // run of tool_result echoes interleaved with two genuine user turns.
        // Only the two real user messages must classify as corrections.
        let msgs = vec![
            assistant("a0"),
            tool_result_user("--- icon def block ---\n39:  const I = {};", false),
            assistant("a1"),
            user("read the deck and make sure the edits land"),
            assistant("a2"),
            tool_result_user(
                "<tool_use_error>File has not been read yet.</tool_use_error>",
                true,
            ),
            assistant("a3"),
            user("yes - clean it up please"),
            assistant("a4"),
            tool_result_user("=== reverted ===\n## main...origin/main", false),
            assistant("a5"),
            tool_result_user("(Bash completed with no output)", false),
            assistant("a6"),
        ];
        let out = detect_corrections(&msgs, "agent-a446ccbb9e59d3bc4");
        assert_eq!(out.len(), 2, "only the two real user turns are corrections");
        assert_eq!(out[0].context, "read the deck and make sure the edits land");
        assert_eq!(out[1].context, "yes - clean it up please");
    }

    // ---------- chat-tuned corrections ----------

    #[test]
    fn chat_corrections_require_corrective_language() {
        // Ordinary short group-chat replies must NOT count as corrections in
        // chat sessions, even though they'd pass the coding length window.
        let msgs = vec![
            assistant("Here is the summary you asked for."),
            user("thanks, that looks really great"),
            assistant("Happy to help."),
        ];
        assert!(detect_corrections_chat(&msgs, "s1").is_empty());
        // The same window IS a correction under the coding heuristic.
        assert_eq!(detect_corrections(&msgs, "s1").len(), 1);
    }

    #[test]
    fn chat_corrections_detect_corrective_markers() {
        let cases = [
            "no jibot, don't answer in that channel",
            "stop replying to every message",
            "that's not what the group decided",
            "wrong group — that was for the steering committee",
            "you should never post invoices here",
        ];
        for text in cases {
            let msgs = vec![assistant("a"), user(text), assistant("b")];
            let out = detect_corrections_chat(&msgs, "s1");
            assert_eq!(out.len(), 1, "expected chat correction for: {text}");
            assert_eq!(out[0].context, text);
        }
    }

    #[test]
    fn chat_corrections_ignore_conversational_marker_lookalikes() {
        // Broad-marker false positives: ordinary chat that merely contains
        // stop/never/instead/actually must not fire.
        let cases = [
            "let's stop by the cafe after the session",
            "I've never been to that part of town",
            "let's meet on Zoom instead of in person",
            "actually, that sounds great to me",
        ];
        for text in cases {
            let msgs = vec![assistant("a"), user(text), assistant("b")];
            assert!(
                detect_corrections_chat(&msgs, "s1").is_empty(),
                "false-positive chat correction for: {text}"
            );
        }
    }

    #[test]
    fn chat_corrections_anchor_matches_after_leading_whitespace() {
        let msgs = vec![
            assistant("a"),
            user("  no, that message was for the other group"),
            assistant("b"),
        ];
        assert_eq!(detect_corrections_chat(&msgs, "s1").len(), 1);
    }

    #[test]
    fn chat_corrections_keep_length_window_and_tool_result_filter() {
        // Too short even with a marker.
        let msgs = vec![assistant("a"), user("no, stop"), assistant("b")];
        assert!(detect_corrections_chat(&msgs, "s1").is_empty());
        // Tool echoes stay excluded.
        let msgs = vec![
            assistant("a"),
            tool_result_user("<tool_use_error>don't do that, wrong file</tool_use_error>", true),
            assistant("b"),
        ];
        assert!(detect_corrections_chat(&msgs, "s1").is_empty());
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

    // ---------- expected-noise filter (jilog#42fd) ----------

    fn errors_for(name: &str, content: serde_json::Value) -> Vec<ErrorSignal> {
        detect_errors(&[tool(name, content)], "s1")
    }

    #[test]
    fn errors_skip_mode_denial_status() {
        // jilog#02j8 shape.
        let c = json!({"error": null, "output": {"status": "denied", "user_instruction": "Inform the user: I'd like to clear the current mode"}, "success": false});
        assert!(errors_for("mode", c).is_empty());
    }

    #[test]
    fn errors_skip_mode_denied_mode() {
        // jilog#gahn shape.
        let c = json!({"error": null, "output": {"denied_mode": "debug", "status": "denied"}, "success": false});
        assert!(errors_for("mode", c).is_empty());
    }

    #[test]
    fn errors_skip_mode_clear_denied_code() {
        // jilog#qryp shape: structured error with a *_denied code.
        let c = json!({"success": false, "error": {"code": "clear_denied", "message": "Cannot clear mode while in 'debug'."}});
        assert!(errors_for("mode", c).is_empty());
    }

    #[test]
    fn errors_skip_mode_flat_denied_code() {
        let c = json!({"code": "switch_denied", "success": false});
        assert!(errors_for("mode", c).is_empty());
    }

    #[test]
    fn errors_keep_mode_non_denial() {
        let c = json!({"success": false, "error": {"code": "invalid_transition", "message": "no such mode"}});
        let out = errors_for("mode", c);
        assert_eq!(out.len(), 1, "a mode error that is not a denial must still emit");
        assert!(out[0].message.contains("invalid_transition"));
    }

    #[test]
    fn errors_keep_denied_status_from_other_tool() {
        // The denial filter is scoped to the `mode` tool.
        let c = json!({"error": null, "output": {"status": "denied"}, "success": false});
        assert_eq!(errors_for("delegate", c).len(), 1);
    }

    #[test]
    fn errors_skip_bash_bare_timeout() {
        // jilog#6s9q shape (error object with message) and the string form.
        let obj = json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}});
        assert!(errors_for("bash", obj).is_empty());
        let s = json!({"success": false, "error": "Command timed out after 120 seconds."});
        assert!(errors_for("bash", s).is_empty());
        let top = json!({"success": false, "message": "Command timed out after 25 seconds"});
        assert!(errors_for("bash", top).is_empty());
    }

    #[test]
    fn errors_skip_bash_bare_nonzero_exit() {
        // jilog#dcpg shape: returncode 1, empty stdout and stderr.
        let c = json!({"error": null, "output": {"returncode": 1, "stderr": "", "stdout": ""}, "success": false});
        assert!(errors_for("bash", c).is_empty());
        // Whitespace-only counts as blank; absent fields count as blank.
        let ws = json!({"error": null, "output": {"returncode": 2, "stderr": " \n", "stdout": "\n"}, "success": false});
        assert!(errors_for("bash", ws).is_empty());
        let absent = json!({"output": {"returncode": 127}, "success": false});
        assert!(errors_for("bash", absent).is_empty());
    }

    #[test]
    fn errors_keep_bash_banner_stdout() {
        // jilog#mg3p: stdout is only a heading — still emitted; no marker
        // heuristic decides what is "not diagnostic".
        let c = json!({"error": null, "output": {"returncode": 1, "stderr": "", "stdout": "=== today's health report ===\n"}, "success": false});
        assert_eq!(errors_for("bash", c).len(), 1);
    }

    #[test]
    fn errors_keep_bash_stderr() {
        let c = json!({"error": null, "output": {"returncode": 1, "stderr": "kata-dispatch: kata show failed", "stdout": ""}, "success": false});
        assert_eq!(errors_for("bash", c).len(), 1);
    }

    #[test]
    fn errors_keep_bash_marker_free_stdout() {
        let c = json!({"error": null, "output": {"returncode": 1, "stderr": "", "stdout": "checksum mismatch: expected X, got Y"}, "success": false});
        assert_eq!(errors_for("bash", c).len(), 1);
    }

    #[test]
    fn errors_keep_bash_structured_stdout_and_stderr() {
        // Non-string stdout/stderr is an unrecognized shape: emit.
        let obj = json!({"error": null, "output": {"returncode": 1, "stderr": "", "stdout": {"failed": ["a"]}}, "success": false});
        assert_eq!(errors_for("bash", obj).len(), 1);
        let arr = json!({"error": null, "output": {"returncode": 1, "stderr": ["boom"], "stdout": ""}, "success": false});
        assert_eq!(errors_for("bash", arr).len(), 1);
    }

    #[test]
    fn errors_keep_bash_extra_fields_anywhere() {
        // Diagnostic content in a field the shapes do not know about must
        // never be discarded (fresheyes 2026-09-03 pass 1): a sibling in
        // output, a sibling beside the timeout message, an extra top-level
        // key. Each is an unrecognized envelope → emitted.
        let in_output = json!({"success": false, "output": {"returncode": 1, "message": "disk full"}});
        assert_eq!(errors_for("bash", in_output).len(), 1);
        let beside_timeout = json!({"success": false, "error": {"message": "Command timed out after 30 seconds", "partial": "tail of log"}});
        assert_eq!(errors_for("bash", beside_timeout).len(), 1);
        let top_level = json!({"success": false, "error": null, "output": {"returncode": 1, "stdout": "", "stderr": ""}, "diagnostics": "oom"});
        assert_eq!(errors_for("bash", top_level).len(), 1);
        let timeout_top = json!({"success": false, "error": "Command timed out after 30 seconds", "stderr": "killed"});
        assert_eq!(errors_for("bash", timeout_top).len(), 1);
        // A top-level `message` beside a non-null `error` is an unread
        // field (Stage 1 round 2): emit, whatever its type.
        let msg_beside_error = json!({"success": false, "error": "Command timed out after 30 seconds", "message": "Traceback (most recent call last): ..."});
        assert_eq!(errors_for("bash", msg_beside_error).len(), 1);
        let msg_beside_error_obj = json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}, "message": "disk full"});
        assert_eq!(errors_for("bash", msg_beside_error_obj).len(), 1);
        let msg_struct = json!({"success": false, "error": "Command timed out after 30 seconds", "message": {"detail": "disk full"}});
        assert_eq!(errors_for("bash", msg_struct).len(), 1);
        // A non-integer returncode beside a timeout is content in an
        // allowed key the timeout shape never reads (fresheyes round 2).
        let rc_text = json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}, "output": {"returncode": "see log: disk full", "stdout": "", "stderr": ""}});
        assert_eq!(errors_for("bash", rc_text).len(), 1);
        let rc_obj = json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}, "output": {"returncode": {"signal": "SIGKILL"}}});
        assert_eq!(errors_for("bash", rc_obj).len(), 1);
        // Present-but-null returncode is "present and not an integer".
        let rc_null = json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}, "output": {"returncode": null, "stdout": "", "stderr": ""}});
        assert_eq!(errors_for("bash", rc_null).len(), 1);
    }

    #[test]
    fn errors_skip_mode_denied_mode_null_value() {
        // Presence of the field is the marker, whatever its value.
        let c = json!({"success": false, "output": {"denied_mode": null}});
        assert!(errors_for("mode", c).is_empty());
    }

    #[test]
    fn errors_skip_bash_production_timeout_envelope() {
        // The shape tool_bash actually emits on a timeout: error.message
        // AND output both carry the sentence (Stage 1 review verified
        // against the tool source and 5/5 transcript samples). This is the
        // jilog#6s9q signal and must be suppressed.
        let real = json!({"error": {"message": "Command timed out after 30 seconds"}, "output": "Command timed out after 30 seconds", "success": false});
        assert!(errors_for("bash", real).is_empty());
        // An empty string output is equally content-free.
        let empty = json!({"success": false, "error": "Command timed out after 30 seconds", "output": ""});
        assert!(errors_for("bash", empty).is_empty());
    }

    #[test]
    fn errors_keep_bash_non_object_output_with_content() {
        // A string output carrying anything but the timeout sentence is
        // content (roborev #1858); an array or number output is an
        // unrecognized envelope. All emit.
        let s = json!({"success": false, "error": "Command timed out after 30 seconds", "output": "partial build log"});
        assert_eq!(errors_for("bash", s).len(), 1);
        let other_sentence = json!({"success": false, "error": "Command timed out after 30 seconds", "output": "Command timed out after 30 seconds\nTraceback (most recent call last):"});
        assert_eq!(errors_for("bash", other_sentence).len(), 1);
        let arr = json!({"success": false, "output": ["returncode", 1]});
        assert_eq!(errors_for("bash", arr).len(), 1);
        let n = json!({"success": false, "error": "Command timed out after 30 seconds", "output": 1});
        assert_eq!(errors_for("bash", n).len(), 1);
        // A string output on the nonzero-exit shape has no returncode to
        // read, so it can never match shape 2 either.
        let s2 = json!({"success": false, "output": "exit status 1"});
        assert_eq!(errors_for("bash", s2).len(), 1);
    }

    #[test]
    fn errors_keep_bash_structured_error() {
        // Error object without a string message, and an error array.
        let obj = json!({"success": false, "error": {"code": "E1"}, "output": {"returncode": 1, "stdout": "", "stderr": ""}});
        assert_eq!(errors_for("bash", obj).len(), 1);
        let arr = json!({"success": false, "error": ["x"], "output": {"returncode": 1, "stdout": "", "stderr": ""}});
        assert_eq!(errors_for("bash", arr).len(), 1);
    }

    #[test]
    fn errors_keep_bash_timeout_with_output() {
        let c = json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}, "output": {"stdout": "partial build log", "stderr": ""}});
        assert_eq!(errors_for("bash", c).len(), 1);
        let e = json!({"success": false, "error": "Command timed out after 30 seconds", "output": {"stdout": "", "stderr": "warning: slow"}});
        assert_eq!(errors_for("bash", e).len(), 1);
    }

    #[test]
    fn errors_keep_bash_other_error_text() {
        let c = json!({"success": false, "error": "spawn failed"});
        assert_eq!(errors_for("bash", c).len(), 1);
        // Timeout sentence with extra words is not the bare sentence.
        let d = json!({"success": false, "error": "Command timed out after 30 seconds while running cargo"});
        assert_eq!(errors_for("bash", d).len(), 1);
    }

    #[test]
    fn errors_keep_bash_unknown_envelope() {
        let c = json!({"success": false, "weird": 1});
        assert_eq!(errors_for("bash", c).len(), 1);
    }

    #[test]
    fn errors_keep_bash_zero_or_non_integer_returncode() {
        let zero = json!({"error": null, "output": {"returncode": 0, "stderr": "", "stdout": ""}, "success": false});
        assert_eq!(errors_for("bash", zero).len(), 1);
        let s = json!({"error": null, "output": {"returncode": "1", "stderr": "", "stdout": ""}, "success": false});
        assert_eq!(errors_for("bash", s).len(), 1);
    }

    #[test]
    fn errors_keep_bare_shapes_from_other_tools() {
        // The bash shapes are scoped to the `bash` tool.
        let c = json!({"error": null, "output": {"returncode": 1, "stderr": "", "stdout": ""}, "success": false});
        assert_eq!(errors_for("python_check", c).len(), 1);
    }

    #[test]
    fn error_text_classification() {
        assert_eq!(error_text(&json!({})), ErrText::Absent);
        assert_eq!(error_text(&json!({"error": null})), ErrText::Absent);
        assert_eq!(error_text(&json!({"error": "x"})), ErrText::Text("x".into()));
        assert_eq!(error_text(&json!({"error": {"message": "m"}})), ErrText::Text("m".into()));
        assert_eq!(error_text(&json!({"message": "top"})), ErrText::Text("top".into()));
        assert_eq!(error_text(&json!({"error": {"code": "c"}})), ErrText::Unrecognized);
        assert_eq!(error_text(&json!({"error": 7})), ErrText::Unrecognized);
        assert_eq!(error_text(&json!({"message": 7})), ErrText::Unrecognized);
        assert_eq!(error_text(&json!({"error": "x", "message": "y"})), ErrText::Unrecognized);
        assert_eq!(error_text(&json!({"error": null, "message": "y"})), ErrText::Text("y".into()));
        // A null message is absent (roborev #1865).
        assert_eq!(error_text(&json!({"error": "x", "message": null})), ErrText::Text("x".into()));
        assert!(errors_for("bash", json!({"success": false, "error": {"message": "Command timed out after 30 seconds"}, "message": null, "output": ""})).is_empty());
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

    // ---------- deferrals ----------

    #[test]
    fn deferrals_basic_match() {
        let msgs = vec![assistant("I'll come back to this after the tests pass.")];
        let out = detect_deferrals(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "s1");
        assert_eq!(out[0].item, "come back later");
    }

    #[test]
    fn deferrals_short_text_detected() {
        let msgs = vec![assistant("next session")];
        let out = detect_deferrals(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item, "next session");
    }

    #[test]
    fn deferrals_first_match_wins() {
        let msgs = vec![assistant("I'll come back to this next session.")];
        let out = detect_deferrals(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item, "come back later");
    }

    #[test]
    fn deferrals_user_role_skipped() {
        let msgs = vec![user("please punt on this until next session")];
        assert!(detect_deferrals(&msgs, "s1").is_empty());
    }

    #[test]
    fn deferrals_tool_blocks_excluded() {
        let msgs = vec![Message {
            role: Some("assistant".into()),
            content: Some(json!([
                {"type": "tool_use", "name": "bash", "input": {"note": "next session"}},
                {"type": "text", "text": "I ran the command."},
            ])),
            name: None,
        }];
        let out = detect_deferrals(&msgs, "s1");
        assert!(out.is_empty(), "tool_use blocks must be skipped");
    }

    #[test]
    fn deferrals_no_match_returns_empty() {
        let msgs = vec![assistant("All requested work is complete.")];
        assert!(detect_deferrals(&msgs, "s1").is_empty());
    }

    #[test]
    fn deferrals_label_emitted_correctly() {
        let msgs = vec![assistant("Let me defer that until the next pass.")];
        let out = detect_deferrals(&msgs, "s1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item, "defer");
    }

    #[test]
    fn deferrals_overlap_with_workaround_both_fire() {
        let msgs = vec![assistant(
            "Skipping for now while we validate the migration.",
        )];
        let workarounds = detect_workarounds(&msgs, "s1");
        let deferrals = detect_deferrals(&msgs, "s1");
        assert_eq!(workarounds.len(), 1);
        assert_eq!(workarounds[0].pattern, "for now");
        assert_eq!(deferrals.len(), 1);
        assert_eq!(deferrals[0].item, "skipping for now");
    }

    // ---------- p0 alerts ----------

    #[test]
    fn p0_alerts_threshold_three_distinct() {
        let errors = vec![
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "x".into(), ..Default::default() },
            ErrorSignal { session_id: "b".into(), tool_name: "bash".into(), message: "y".into(), ..Default::default() },
            ErrorSignal { session_id: "c".into(), tool_name: "bash".into(), message: "z".into(), ..Default::default() },
        ];
        let p0 = detect_p0_alerts(&errors);
        assert_eq!(p0.len(), 1);
        assert_eq!(p0["bash"].len(), 3);
    }

    #[test]
    fn p0_alerts_two_distinct_skipped() {
        let errors = vec![
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "x".into(), ..Default::default() },
            ErrorSignal { session_id: "b".into(), tool_name: "bash".into(), message: "y".into(), ..Default::default() },
        ];
        assert!(detect_p0_alerts(&errors).is_empty());
    }

    #[test]
    fn p0_alerts_same_session_repeated_doesnt_count_twice() {
        let errors = vec![
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "1".into(), ..Default::default() },
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "2".into(), ..Default::default() },
            ErrorSignal { session_id: "a".into(), tool_name: "bash".into(), message: "3".into(), ..Default::default() },
        ];
        // Only 1 distinct session → below threshold
        assert!(detect_p0_alerts(&errors).is_empty());
    }

    #[test]
    fn p0_alerts_subagent_sessions_excluded() {
        let errors = vec![
            ErrorSignal { session_id: "00000000000000001".into(), tool_name: "bash".into(), message: "x".into(), ..Default::default() },
            ErrorSignal { session_id: "00000000000000002".into(), tool_name: "bash".into(), message: "y".into(), ..Default::default() },
            ErrorSignal { session_id: "00000000000000003".into(), tool_name: "bash".into(), message: "z".into(), ..Default::default() },
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
