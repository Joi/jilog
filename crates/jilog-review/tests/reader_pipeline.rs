//! Integration tests: fixture events.jsonl → reader → run_review → digest.
//!
//! One test per reader that implements `Reader::load_events`, per the
//! 2026-07-05 brush-up design's Testing section: the digest produced from a
//! fixture event stream must contain a populated Patterns section.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Duration, NaiveDate, Utc};

use jilog_review::readers::{AmplifierReader, ContextIntelligenceReader};
use jilog_review::trackers::NoneTracker;
use jilog_review::{Reader, ReviewArgs, run_review};

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("jilog-test-pipeline").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Event lines exhibiting a compaction storm (3 compactions in 8 minutes),
/// a stuck loop (bash x4 identical arguments), priced usage on the
/// llm:response, and enough chat traffic to also produce a user/assistant
/// exchange. `workspace` is present on every line (the amplifier parser
/// ignores it, the CI stream requires it).
fn storm_fixture_lines() -> String {
    let mut lines = vec![
        r#"{"data":{"prompt":"please fix the build"},"event":"prompt:submit","timestamp":"2026-07-01T09:00:00+00:00","workspace":"w"}"#.to_string(),
        r#"{"data":{"model":"claude-opus-4-8","raw":{"content":[{"text":"on it","type":"text"}]},"usage":{"cost_usd":0.42,"input_tokens":1000,"output_tokens":100}},"event":"llm:response","timestamp":"2026-07-01T09:00:05+00:00","workspace":"w"}"#.to_string(),
        r#"{"data":{},"event":"context:compaction","timestamp":"2026-07-01T09:01:00+00:00","workspace":"w"}"#.to_string(),
        r#"{"data":{},"event":"context:compaction","timestamp":"2026-07-01T09:04:00+00:00","workspace":"w"}"#.to_string(),
        r#"{"data":{},"event":"context:compaction","timestamp":"2026-07-01T09:08:00+00:00","workspace":"w"}"#.to_string(),
    ];
    for i in 0..4 {
        lines.push(format!(
            r#"{{"data":{{"tool_input":{{"command":"cargo build"}},"tool_name":"bash"}},"event":"tool:pre","timestamp":"2026-07-01T09:{:02}:00+00:00","workspace":"w"}}"#,
            10 + i
        ));
    }
    lines.join("\n") + "\n"
}

fn run_pipeline(reader: Box<dyn Reader>, digest_dir: &Path) -> String {
    let readers = vec![reader];
    let args = ReviewArgs {
        since: Utc::now() - Duration::days(3650),
        digest_dir: digest_dir.to_path_buf(),
        processed_file: None,
        date: NaiveDate::from_ymd_opt(2026, 7, 5).unwrap(),
        dry_run: false,
        create_issues: false,
    };
    let report = run_review(&readers, &NoneTracker, &args).unwrap();
    assert_eq!(report.sessions_scanned, 1, "fixture session must be scanned");
    fs::read_to_string(&report.digest_path).unwrap()
}

#[test]
fn amplifier_events_fixture_produces_pattern_section() {
    let root = test_dir("amplifier-patterns");
    let sess = root.join("proj").join("sessions").join("sess-storm");
    fs::create_dir_all(&sess).unwrap();
    fs::write(sess.join("events.jsonl"), storm_fixture_lines()).unwrap();

    let digest_dir = root.join("digests");
    let body = run_pipeline(Box::new(AmplifierReader::new(&root)), &digest_dir);

    assert!(body.contains("## Patterns"), "digest:\n{}", body);
    assert!(
        body.contains("- `sess-storm` kind=`compaction_storm`: 3 compactions 09:01-09:08"),
        "digest:\n{}",
        body
    );
    assert!(
        body.contains("- `sess-storm` kind=`stuck_loop`: `bash` x4 identical arguments 09:10-09:13"),
        "digest:\n{}",
        body
    );
    assert!(body.contains("## Spend"), "digest:\n{}", body);
    assert!(
        body.contains("- **Total**: $0.42 across 1 of 1 session(s) with usage data"),
        "digest:\n{}",
        body
    );
    assert!(body.contains("- `claude-opus-4-8`: $0.42"), "digest:\n{}", body);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn context_intelligence_events_fixture_produces_pattern_section() {
    let root = test_dir("ci-patterns");
    let ci = root
        .join("proj")
        .join("sessions")
        .join("sess-ci-storm")
        .join("context-intelligence");
    fs::create_dir_all(&ci).unwrap();
    fs::write(
        ci.join("metadata.json"),
        r#"{"format":"context-intelligence","version":"1.0.0","last_event_at":"2026-07-01T09:13:00+00:00"}"#,
    )
    .unwrap();
    fs::write(ci.join("events.jsonl"), storm_fixture_lines()).unwrap();

    let digest_dir = root.join("digests");
    let body = run_pipeline(Box::new(ContextIntelligenceReader::new(&root)), &digest_dir);

    assert!(body.contains("## Patterns"), "digest:\n{}", body);
    assert!(
        body.contains("- `sess-ci-storm` kind=`compaction_storm`: 3 compactions 09:01-09:08"),
        "digest:\n{}",
        body
    );
    assert!(
        body.contains("- `sess-ci-storm` kind=`stuck_loop`: `bash` x4 identical arguments 09:10-09:13"),
        "digest:\n{}",
        body
    );
    // The same fixture also flows through the message path (frontmatter counts it).
    assert!(body.contains("patterns: 2"), "digest:\n{}", body);
    assert!(body.contains("## Spend"), "digest:\n{}", body);
    assert!(
        body.contains("- **Total**: $0.42 across 1 of 1 session(s) with usage data"),
        "digest:\n{}",
        body
    );
    assert!(body.contains("- **Tokens**: 1000 in / 100 out"), "digest:\n{}", body);
    let _ = fs::remove_dir_all(&root);
}
