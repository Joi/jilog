//! Stamp the binary with the git commit it was built from, so
//! `jilog --version` names it (jibot-code#hw8a).
//!
//! Fleet Macs install with `cargo install --path`, which records only the
//! path in ~/.cargo/.crates.toml. Without the commit in the binary nobody
//! could tell that macazbd ran a 2026-11-02 build three weeks behind main.
//!
//! JILOG_GIT_COMMIT is the full 40-hex HEAD, with `-dirty` appended when
//! tracked files differ from HEAD, or `unknown` when the source is not a git
//! checkout (a crates.io tarball) or git is missing. An explicit
//! JILOG_GIT_COMMIT in the build environment wins, for packagers.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=JILOG_GIT_COMMIT");
    if let Ok(v) = std::env::var("JILOG_GIT_COMMIT") {
        if !v.is_empty() {
            println!("cargo:rustc-env=JILOG_GIT_COMMIT={v}");
            println!("cargo:rustc-env=JILOG_GIT_COMMIT_OVERRIDDEN=1");
            return;
        }
    }

    let commit = match git(&["rev-parse", "--verify", "HEAD"]) {
        Some(sha) if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) => {
            // `diff-index` exits 1 when tracked files differ from HEAD.
            // Refresh the index first so a touched-but-unchanged file does
            // not read as dirty.
            let _ = git(&["update-index", "-q", "--refresh"]);
            let clean = Command::new("git")
                .args(["diff-index", "--quiet", "HEAD", "--"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if clean { sha } else { format!("{sha}-dirty") }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=JILOG_GIT_COMMIT={commit}");

    // Rebuild when HEAD moves or the index changes. Paths come from git so a
    // linked worktree (where .git is a file) resolves correctly.
    // A path that does not exist would make cargo rerun on every build, so
    // only existing ones are watched.
    //
    // Git metadata alone misses an unstaged edit: the index does not change,
    // so a reused target dir (cargo install --path keeps the workspace's)
    // would stamp a modified build with the clean SHA. Watching the
    // workspace sources reruns this script, and so recomputes the dirty
    // flag, whenever a tracked source file changes. Cargo scans a watched
    // directory recursively by mtime.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    if let Some(ws) = manifest.parent().and_then(|p| p.parent()) {
        for rel in ["crates", "Cargo.toml", "Cargo.lock"] {
            let path = ws.join(rel);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    let mut watch: Vec<String> = vec!["HEAD".into(), "index".into(), "packed-refs".into()];
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(r);
    }
    for p in &watch {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", p]) {
            if PathBuf::from(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
}
