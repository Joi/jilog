//! `jilog review nightly --json` end to end: stdout is one JSON document
//! even when a workaround register cannot be read and a warning is logged
//! (jibot-code#x9k9 review).

use std::process::Command;

#[test]
fn json_stdout_parses_with_an_unreadable_register() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("jilog.toml");
    std::fs::write(
        &cfg,
        "workaround_registers = [\"/nonexistent/jilog/x9k9-register.md\"]\n\
         [[reader]]\ntype = \"pi\"\npath = \"/nonexistent/jilog/pi\"\n\
         [tracker]\ntype = \"none\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jilog"))
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "review",
            "nightly",
            "--dry-run",
            "--json",
            "--date",
            "2026-11-01",
            "--digest-dir",
            dir.path().join("digests").to_str().unwrap(),
            "--processed-file",
            dir.path().join("processed.txt").to_str().unwrap(),
        ])
        .env("HOME", dir.path())
        .env("RUST_LOG", "warn")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let problems = &value["workaround_registers"]["problems"];
    assert_eq!(problems[0]["register"], "/nonexistent/jilog/x9k9-register.md", "{value}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not checked"), "the warning goes to stderr: {stderr}");
}
