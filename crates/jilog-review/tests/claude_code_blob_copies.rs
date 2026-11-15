//! Claude Code writes an `api-request-blob` line for every API request,
//! repeating the request's system prompt, user turns and assistant turns.
//! Read as chat lines, those copies made every captured signal appear twice
//! in the nightly digest (jibot-code#r1jp, 2026-W46 synthesis). The reader
//! keeps one copy: the `user` and `assistant` lines.
//!
//! The fixtures are the trimmed security-watch sessions of jibot-code#xbpq.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};

use jilog_review::readers::ClaudeCodeReader;
use jilog_review::Reader;

fn text_blocks(content: &Option<serde_json::Value>) -> Vec<String> {
    match content {
        None => Vec::new(),
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Each assistant sentence in the fixtures is said once; the transcript
/// holds it twice (once as `assistant`, once in an `api-request-blob`).
#[test]
fn each_assistant_text_is_loaded_once() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code");
    let reader = ClaudeCodeReader::new(dir);
    let handles = reader.discover(Utc.timestamp_opt(0, 0).unwrap()).unwrap();
    assert_eq!(handles.len(), 2);
    for handle in &handles {
        let raw = std::fs::read_to_string(&handle.path).unwrap();
        assert!(
            raw.contains("\"type\":\"api-request-blob\""),
            "{}: fixture must keep its blob copies",
            handle.session_id
        );

        let messages = reader.load(handle).unwrap();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for msg in messages.iter().filter(|m| m.role.as_deref() == Some("assistant")) {
            for text in text_blocks(&msg.content) {
                *counts.entry(text).or_default() += 1;
            }
        }
        assert!(!counts.is_empty(), "{}: no assistant text loaded", handle.session_id);
        let repeated: Vec<_> = counts.iter().filter(|(_, n)| **n > 1).collect();
        assert!(repeated.is_empty(), "{}: {repeated:#?}", handle.session_id);
        assert!(
            messages.iter().all(|m| m.name.as_deref() != Some("api-request-blob")),
            "{}: a blob copy was loaded",
            handle.session_id
        );
    }
}
