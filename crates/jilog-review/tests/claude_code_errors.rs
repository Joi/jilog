//! Known-positive regression fixtures for the error detector on Claude Code
//! transcripts (jibot-code#xbpq).
//!
//! Both fixtures are real security-watch sessions from 2026-11, trimmed: the
//! prompt and user text are removed and long strings are cut to 240 chars.
//! Every tool call and result keeps its shape, including the
//! `api-request-blob` copies that repeat a turn. The weekly synthesis of
//! 2026-11-15 found each one failing and retrying while the digest reported
//! `errors: 0`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};

use jilog_review::readers::ClaudeCodeReader;
use jilog_review::{detect_errors, ErrorSignal, Reader};

fn fixture_errors() -> BTreeMap<String, Vec<ErrorSignal>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code");
    let reader = ClaudeCodeReader::new(dir);
    let handles = reader.discover(Utc.timestamp_opt(0, 0).unwrap()).unwrap();
    assert_eq!(handles.len(), 2, "both fixtures must be discovered");
    handles
        .iter()
        .map(|h| {
            let messages = reader.load(h).unwrap();
            (h.session_id.clone(), detect_errors(&messages, &h.session_id))
        })
        .collect()
}

/// The post URL hung on every fetch: five distinct WebFetch calls returned
/// `is_error: true` "socket hang up" before the agent fell back to search.
/// The transcript holds nine copies of those results; each call counts once.
#[test]
fn hung_fetch_session_reports_each_failed_call_once() {
    let all = fixture_errors();
    let errors = &all["7649ad4b-efda-4d25-ac75-9861384685c7"];
    assert_eq!(errors.len(), 5, "{errors:#?}");
    for e in errors {
        assert_eq!(e.tool_name, "WebFetch");
        assert_eq!(e.message, "socket hang up");
    }
}

/// The EUVD page loaded (it said the application was down, which is page
/// text, not a tool failure) and vulners.com answered 403. Only the 403 is
/// a failed call.
#[test]
fn forbidden_fetch_session_reports_the_http_error_only() {
    let all = fixture_errors();
    let errors = &all["73fac849-cffe-40f7-9be9-28047ee1e1dc"];
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0].tool_name, "WebFetch");
    assert!(
        errors[0].message.starts_with("The server returned HTTP 403 Forbidden."),
        "{}",
        errors[0].message
    );
}
