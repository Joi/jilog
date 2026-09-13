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
use jilog_review::{run_review, Reader, ReviewArgs};

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
    assert_eq!(
        report.sessions_scanned, 1,
        "fixture session must be scanned"
    );
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
        body.contains(
            "- `sess-storm` kind=`stuck_loop`: `bash` x4 identical arguments 09:10-09:13"
        ),
        "digest:\n{}",
        body
    );
    assert!(body.contains("## Spend"), "digest:\n{}", body);
    assert!(
        body.contains("- **Total**: $0.42 across 1 of 1 session(s) with usage data"),
        "digest:\n{}",
        body
    );
    assert!(
        body.contains("- `claude-opus-4-8`: $0.42"),
        "digest:\n{}",
        body
    );
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
        body.contains(
            "- `sess-ci-storm` kind=`stuck_loop`: `bash` x4 identical arguments 09:10-09:13"
        ),
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
    assert!(
        body.contains("- **Tokens**: 1000 in / 100 out"),
        "digest:\n{}",
        body
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn nanoclaw_cell_fixture_produces_dims_and_pattern_section() {
    use jilog_review::readers::NanoclawReader;

    let root = test_dir("nanoclaw-patterns");

    // Minimal v2.db: one jibot agent serving one WhatsApp group.
    let conn = rusqlite::Connection::open(root.join("v2.db")).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE agent_groups (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, folder TEXT NOT NULL UNIQUE,
            agent_provider TEXT, created_at TEXT NOT NULL
        );
        CREATE TABLE messaging_groups (
            id TEXT PRIMARY KEY, channel_type TEXT NOT NULL, platform_id TEXT NOT NULL,
            instance TEXT NOT NULL, name TEXT, is_group INTEGER DEFAULT 0,
            unknown_sender_policy TEXT NOT NULL DEFAULT 'strict', created_at TEXT NOT NULL,
            denied_at TEXT
        );
        CREATE TABLE messaging_group_agents (
            id TEXT PRIMARY KEY, messaging_group_id TEXT NOT NULL,
            agent_group_id TEXT NOT NULL, session_mode TEXT DEFAULT 'shared',
            priority INTEGER DEFAULT 0, created_at TEXT NOT NULL
        );
        INSERT INTO agent_groups VALUES ('ag-v', 'jibot', 'vibez', NULL, '2026-05-06');
        INSERT INTO messaging_groups VALUES
            ('mg-v', 'whatsapp', '1@g.us', 'whatsapp', 'The vibez', 1, 'public', '2026-05-06', NULL);
        INSERT INTO messaging_group_agents VALUES ('mga-v', 'mg-v', 'ag-v', 'shared', 0, '2026-05-06');
        "#,
    )
    .unwrap();

    // A stuck loop: the same tool called with identical arguments 4 times,
    // plus a user turn with a chat correction.
    let proj = root.join("v2-sessions/ag-v/.claude-shared/projects/-workspace-agent");
    fs::create_dir_all(&proj).unwrap();
    let mut lines = vec![
        r#"{"type":"assistant","uuid":"a-pre","timestamp":"2026-07-01T08:59:00.000Z","message":{"id":"msg_pre","role":"assistant","content":[{"type":"text","text":"Posting the summary here."}]},"sessionId":"sess-cell"}"#.to_string(),
        r#"{"type":"user","uuid":"u1","timestamp":"2026-07-01T09:00:00.000Z","message":{"role":"user","content":"<message id=\"1\" from=\"mg-v\">no jibot, don't answer in that channel</message>"},"sessionId":"sess-cell"}"#.to_string(),
    ];
    for i in 0..4 {
        lines.push(format!(
            r#"{{"type":"assistant","uuid":"a{i}","timestamp":"2026-07-01T09:{:02}:00.000Z","message":{{"id":"msg_{i}","role":"assistant","content":[{{"type":"text","text":"retrying"}},{{"type":"tool_use","id":"toolu_{i}","name":"bash","input":{{"command":"cargo build"}}}}],"usage":{{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":5}}}},"sessionId":"sess-cell"}}"#,
            10 + i
        ));
    }
    fs::write(proj.join("sess-cell.jsonl"), lines.join("\n") + "\n").unwrap();

    let digest_dir = root.join("digests");
    let body = run_pipeline(Box::new(NanoclawReader::new(&root)), &digest_dir);

    // Health pattern detected from cell events, with the dims prefix.
    assert!(body.contains("## Patterns"), "digest:\n{}", body);
    assert!(
        body.contains("- `jibot@The vibez` `sess-cell` kind=`stuck_loop`: `bash` x4 identical arguments 09:10-09:13"),
        "digest:\n{}",
        body
    );
    // Chat correction stamped and prefixed.
    assert!(
        body.contains("- `jibot@The vibez` `sess-cell` — "),
        "digest:\n{}",
        body
    );
    // Personas rollup section.
    assert!(body.contains("## Personas"), "digest:\n{}", body);
    assert!(
        body.contains("- `jibot@The vibez`: 1 corrections, 0 errors, 0 workarounds, 0 deferrals, 1 patterns (1 session(s))"),
        "digest:\n{}",
        body
    );
    // Token-only spend (cells carry no cost field).
    assert!(
        body.contains("- **Tokens**: 40 in / 20 out"),
        "digest:\n{}",
        body
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn pooled_codex_seat_reaches_signals_without_chat_heuristics() {
    use jilog_review::readers::CodexReader;
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("profiles/codex-17/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-seat-test.jsonl"),
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"A temporary workaround is in place.\"}]}}\n"
    ).unwrap();
    let args = ReviewArgs {
        since: Utc::now() - Duration::days(1),
        digest_dir: dir.path().join("digest"),
        processed_file: Some(dir.path().join("processed")),
        date: Utc::now().date_naive(),
        dry_run: false,
        create_issues: false,
    };
    let readers: Vec<Box<dyn Reader>> = vec![Box::new(CodexReader::new(sessions))];
    let report = run_review(&readers, &NoneTracker, &args).unwrap();
    assert!(!report.workarounds.is_empty());
    assert!(report
        .workarounds
        .iter()
        .all(|s| s.seat.as_deref() == Some("codex-17") && s.persona.is_none()));
    assert!(fs::read_to_string(&report.digest_path)
        .unwrap()
        .contains("seat:codex-17"));
    assert!(report.personas.is_empty());
    assert_eq!(
        run_review(&readers, &NoneTracker, &args)
            .unwrap()
            .sessions_scanned,
        0
    );
}

#[cfg(unix)]
#[test]
fn worker_evidence_flows_through_errors_and_dedup() {
    use jilog_review::readers::WorkerSignalsReader;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("kata-fixture");
    fs::write(
        &bin,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1" in
list) [ "$*" = "list --all --status all --meta dispatch --limit 0 --json" ] || exit 9
      cat "$(dirname "$0")/list.json" ;;
show) [ "$*" = "show fixture#abcd --json" ] || exit 4
      cat "$(dirname "$0")/show.json" ;;
*) exit 9 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let issue = json!({"uid":"fixture-uid", "qualified_id":"fixture#abcd", "metadata": {
        "dispatch":{"id":"fixture-dispatch", "harness":"codex", "seat":"codex-01", "dispatched_at":"2026-09-13T00:00:00Z"},
        "kickoff":{"id":"fixture-dispatch", "state":"failed", "detail":"dialog:dir-trust: wait", "at":"2026-09-13T00:01:00Z"}
    }});
    let mut old = issue.clone();
    old["qualified_id"] = json!("fixture#old");
    old["updated_at"] = json!("2020-01-01T00:00:00Z");
    old["metadata"]["dispatch"]["dispatched_at"] = json!("2020-01-01T00:00:00Z");
    old["metadata"]["kickoff"]["at"] = json!("2020-01-01T00:00:00Z");
    let mut unavailable = issue.clone();
    unavailable["qualified_id"] = json!("fixture#gone");
    fs::write(
        dir.path().join("list.json"),
        json!({"issues":[old, unavailable, issue.clone()]}).to_string(),
    )
    .unwrap();
    let mut full = json!({"issue":issue, "comments":[]});
    fs::write(dir.path().join("show.json"), full.to_string()).unwrap();
    let mut reader = WorkerSignalsReader::default();
    reader.kata_bin = bin;
    reader.pool_dir = dir.path().join("pool");
    let readers: Vec<Box<dyn Reader>> = vec![Box::new(reader)];
    let args = ReviewArgs {
        since: chrono::DateTime::parse_from_rfc3339("2026-09-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        digest_dir: dir.path().join("digest"),
        processed_file: Some(dir.path().join("processed")),
        date: NaiveDate::from_ymd_opt(2026, 9, 13).unwrap(),
        dry_run: false,
        create_issues: false,
    };
    let report = run_review(&readers, &NoneTracker, &args).unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].tool_name, "codex_trust_prompt");
    let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
    assert!(calls.contains("show fixture#abcd --json"));
    assert!(calls.contains("show fixture#gone --json"));
    assert!(!calls.contains("fixture#old"));
    assert_eq!(report.errors[0].seat.as_deref(), Some("codex-01"));
    assert_eq!(
        run_review(&readers, &NoneTracker, &args)
            .unwrap()
            .errors
            .len(),
        0
    );
    full["comments"] =
        json!([{"body":"review: fresheyes --gpt", "created_at":"2026-09-13T00:02:00Z"}]);
    fs::write(dir.path().join("show.json"), full.to_string()).unwrap();
    let report = run_review(&readers, &NoneTracker, &args).unwrap();
    assert_eq!(
        report.errors.len(),
        1,
        "new kind after first scan must survive dispatch dedup"
    );
    assert_eq!(report.errors[0].tool_name, "same_model_review");
}

#[cfg(unix)]
#[test]
fn missing_hook_state_flows_through_the_reader() {
    use jilog_review::readers::WorkerSignalsReader;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("kata-fixture");
    fs::write(
        &bin,
        r#"#!/bin/sh
case "$1" in
list) cat "$(dirname "$0")/list.json" ;;
show) cat "$(dirname "$0")/show.json" ;;
*) exit 9 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();

    let worktree = dir.path().join("worktree");
    let state_dir = dir.path().join("state");
    let seat = dir.path().join("pool/profiles/codex-01");
    let day = seat.join("sessions/2026/09/13");
    for path in [&worktree, &state_dir, &day] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(
        seat.join("config.toml"),
        "[hooks.state.\"h:session_start:0:0\"]\ntrusted_hash = \"sha256:a\"\n",
    )
    .unwrap();
    let rollout = [
        json!({"timestamp":"2026-09-13T00:00:30Z","type":"session_meta",
               "payload":{"cwd": worktree, "timestamp":"2026-09-13T00:00:30Z"}})
        .to_string(),
        json!({"timestamp":"2026-09-13T00:01:00Z","type":"response_item","payload":{
               "type":"message","role":"assistant",
               "content":[{"type":"output_text","text":"working"}]}})
        .to_string(),
    ]
    .join("\n");
    fs::write(day.join("rollout-2026-09-13T00-00-30-s1.jsonl"), rollout).unwrap();

    let issue = json!({"uid":"issue-uid", "qualified_id":"jilog#4nd2", "metadata": {
        "dispatch":{"id":"dispatch-1","harness":"codex","seat":"codex-01","host":"macazbd",
            "pane":"%542","tmux_socket":"default","worktree": worktree,
            "dispatched_at":"2026-09-13T00:00:00Z"}
    }});
    fs::write(
        dir.path().join("list.json"),
        json!({"issues":[issue.clone()]}).to_string(),
    )
    .unwrap();
    // `kata show --json` does not echo qualified_id; the reader must carry the
    // reference it listed, because the hook's filename slug comes from it.
    let mut shown = issue.clone();
    shown.as_object_mut().unwrap().remove("qualified_id");
    fs::write(
        dir.path().join("show.json"),
        json!({"issue": shown, "comments": []}).to_string(),
    )
    .unwrap();

    let mut reader = WorkerSignalsReader::default();
    reader.kata_bin = bin;
    reader.pool_dir = dir.path().join("pool");
    reader.state_dir = state_dir.clone();
    reader.codex_home = dir.path().join(".codex");
    reader.host = Some("macazbd".into());
    let readers: Vec<Box<dyn Reader>> = vec![Box::new(reader)];
    let args = |name: &str| ReviewArgs {
        since: chrono::DateTime::parse_from_rfc3339("2026-09-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        digest_dir: dir.path().join("digest"),
        processed_file: Some(dir.path().join(name)),
        date: NaiveDate::from_ymd_opt(2026, 9, 13).unwrap(),
        dry_run: false,
        create_issues: false,
    };
    let report = run_review(&readers, &NoneTracker, &args("processed-1")).unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].tool_name, "codex_missing_hook_state");
    assert_eq!(report.errors[0].seat.as_deref(), Some("codex-01"));

    // The hook's own file, once present, retires the finding.
    fs::write(state_dir.join("jilog-4nd2-default-542.json"), "{}").unwrap();
    assert!(run_review(&readers, &NoneTracker, &args("processed-2"))
        .unwrap()
        .errors
        .is_empty());
}
