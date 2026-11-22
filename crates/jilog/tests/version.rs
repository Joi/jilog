//! `jilog --version` names the commit the binary was built from, in the
//! shape scripts/install and the fleet-tools drift watch parse
//! (jibot-code#hw8a).

use std::process::Command;

#[test]
fn version_names_the_build_commit() {
    let out = Command::new(env!("CARGO_BIN_EXE_jilog"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let prefix = format!("jilog {} (commit ", env!("CARGO_PKG_VERSION"));
    let rest = line
        .strip_prefix(&prefix)
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_else(|| panic!("unexpected version line: {line}"));
    let sha = rest.strip_suffix("-dirty").unwrap_or(rest);
    assert!(
        sha == "unknown" || (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())),
        "commit field is neither 40-hex nor unknown: {line}"
    );
    // The build script and the binary agree.
    assert_eq!(rest, env!("JILOG_GIT_COMMIT"));

    // Independent of the build script: inside a git checkout the stamp is
    // the checkout's HEAD, never `unknown` (a build script that always
    // emitted `unknown` or a fixed SHA would fail here). A packager's
    // explicit JILOG_GIT_COMMIT is the one exception.
    if option_env!("JILOG_GIT_COMMIT_OVERRIDDEN").is_some() {
        return;
    }
    let head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(head) = head {
        assert_eq!(sha, head, "stamp is not the checkout's HEAD: {line}");
    }
}
