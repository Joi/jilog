//! `jilog config check` end to end: the subcommand spelling and the exit
//! status (jilog#1xmf).

use std::process::Command;

fn run_check(toml: &str) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jilog.toml");
    std::fs::write(&path, toml).unwrap();
    Command::new(env!("CARGO_BIN_EXE_jilog"))
        .args(["--config", path.to_str().unwrap(), "config", "check"])
        .output()
        .unwrap()
}

#[test]
fn kata_tracker_warns_and_exits_1() {
    let out = run_check("[[reader]]\ntype = \"claude-code\"\n[tracker]\ntype = \"kata\"\nproject = \"jilog\"\n");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("warn: tracker kata files every raw signal"), "{stdout}");
    assert!(stdout.contains("errors detector: no configured reader feeds it"), "{stdout}");
}

#[test]
fn none_tracker_exits_0() {
    let out = run_check("[[reader]]\ntype = \"pi\"\n[tracker]\ntype = \"none\"\n");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tracker: none"), "{stdout}");
    assert!(stdout.contains("patterns detector: fed by pi"), "{stdout}");
}
