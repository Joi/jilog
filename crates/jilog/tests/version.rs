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
}
