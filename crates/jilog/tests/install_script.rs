//! scripts/install end to end against a scratch remote and a fake jilog
//! binary (jibot-code#hw8a): the --check verdicts and exit codes, the install
//! root it reads, and the refusals that stop before cargo runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/install");

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    checkout: PathBuf,
    root: PathBuf,
    main: String,
}

/// A bare remote with one commit on main, a clone of it holding a copy of
/// scripts/install, and an empty install root.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(seed.join("scripts")).unwrap();
    std::fs::copy(SCRIPT, seed.join("scripts/install")).unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "seed"]);
    let remote = tmp.path().join("remote.git");
    git(tmp.path(), &["clone", "-q", "--bare", seed.to_str().unwrap(), remote.to_str().unwrap()]);
    let checkout = tmp.path().join("jilog");
    git(tmp.path(), &["clone", "-q", remote.to_str().unwrap(), checkout.to_str().unwrap()]);
    let main = git(&checkout, &["rev-parse", "origin/main"]);
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    Fixture { _tmp: tmp, checkout, root, main }
}

impl Fixture {
    fn fake_jilog(&self, version_line: &str) {
        let bin = self.root.join("bin/jilog");
        std::fs::write(&bin, format!("#!/bin/sh\necho '{version_line}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(self.checkout.join("scripts/install"))
            .args(args)
            .env("CARGO_INSTALL_ROOT", &self.root)
            .env("CARGO_HOME", self.root.join("unused-cargo-home"))
            // No cargo on PATH: an install that got past its refusals would
            // fail with exit 2 rather than build anything.
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn check_current_when_the_installed_commit_is_origin_main() {
    let f = fixture();
    f.fake_jilog(&format!("jilog 0.9.0 (commit {})", f.main));
    let o = f.run(&["--check"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).starts_with(&format!("jilog: CURRENT {}", f.main)), "{}", stdout(&o));
}

#[test]
fn check_stale_for_another_commit_a_dirty_build_and_a_build_without_one() {
    let f = fixture();
    for line in [
        "jilog 0.9.0 (commit 0000000000000000000000000000000000000000)".to_string(),
        format!("jilog 0.9.0 (commit {}-dirty)", f.main),
        "jilog 0.9.0".to_string(),
    ] {
        f.fake_jilog(&line);
        let o = f.run(&["--check", "--no-fetch"]);
        assert_eq!(o.status.code(), Some(1), "{line}: {}{}", stdout(&o), stderr(&o));
        assert!(stdout(&o).starts_with("jilog: STALE"), "{line}: {}", stdout(&o));
    }
}

#[test]
fn check_reads_the_install_root_and_reports_absent() {
    let f = fixture();
    // A jilog under CARGO_HOME must not count: CARGO_INSTALL_ROOT wins.
    let o = f.run(&["--check"]);
    assert_eq!(o.status.code(), Some(2), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains(&format!("ABSENT no {}", f.root.join("bin/jilog").display())), "{}", stdout(&o));
}

#[test]
fn install_refuses_a_main_ahead_of_origin_main() {
    let f = fixture();
    std::fs::write(f.checkout.join("local.txt"), "x").unwrap();
    git(&f.checkout, &["add", "local.txt"]);
    git(&f.checkout, &["commit", "-q", "-m", "local only"]);
    let o = f.run(&[]);
    assert_eq!(o.status.code(), Some(1), "{}{}", stdout(&o), stderr(&o));
    assert!(stderr(&o).contains("ahead of origin/main"), "{}", stderr(&o));
}

#[test]
fn install_refuses_a_branch_other_than_main_and_a_dirty_tree() {
    let f = fixture();
    git(&f.checkout, &["checkout", "-q", "-b", "feature"]);
    let o = f.run(&[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("not main"), "{}", stderr(&o));

    git(&f.checkout, &["checkout", "-q", "main"]);
    // A trailing comment dirties the tree without changing what runs.
    std::fs::OpenOptions::new()
        .append(true)
        .open(f.checkout.join("scripts/install"))
        .and_then(|mut fh| std::io::Write::write_all(&mut fh, b"\n# dirty\n"))
        .unwrap();
    let o = f.run(&[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("uncommitted changes"), "{}", stderr(&o));
}
