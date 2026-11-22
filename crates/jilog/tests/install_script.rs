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

    /// A `cargo` stub on PATH that logs its arguments and "installs" a jilog
    /// under the --root it was given, naming the checkout's HEAD. Without
    /// --root it installs nothing, so the script's own check fails. Like real
    /// cargo, it refuses to replace a bin/jilog it has no record of
    /// ($root/.crates.toml) unless given --force (jibot-code#q8kf).
    fn stub_cargo(&self) -> (PathBuf, PathBuf) {
        let dir = self._tmp.path().join("stubbin");
        std::fs::create_dir_all(&dir).unwrap();
        let log = self._tmp.path().join("cargo-args");
        let stub = dir.join("cargo");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/bash\necho \"$*\" > '{log}'\nroot=\"\"; path=\"\"; force=0\n\
                 while [ $# -gt 0 ]; do case \"$1\" in --root) root=\"$2\"; shift;; --path) path=\"$2\"; shift;; --force) force=1;; esac; shift; done\n\
                 [ -n \"$root\" ] || exit 0\n\
                 if [ -e \"$root/bin/jilog\" ] && [ ! -e \"$root/.crates.toml\" ] && [ $force = 0 ]; then\n\
                 echo 'error: binary `jilog` already exists in destination' >&2; exit 101; fi\nsha=$(git -C \"${{path%/crates/jilog}}\" rev-parse HEAD)\nmkdir -p \"$root/bin\"\n\
                 printf '#!/bin/sh\\necho \"jilog 0.9.0 (commit %s)\"\\n' \"$sha\" > \"$root/bin/jilog\"\nchmod +x \"$root/bin/jilog\"\ntouch \"$root/.crates.toml\"\n",
                log = log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, log)
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

#[test]
fn install_passes_its_root_to_cargo_and_verifies_the_result() {
    let f = fixture();
    let (stubbin, log) = f.stub_cargo();
    let path = format!("{}:/usr/bin:/bin", stubbin.display());

    // CARGO_INSTALL_ROOT set: that is the root.
    let o = Command::new("bash")
        .arg(f.checkout.join("scripts/install"))
        .env("CARGO_INSTALL_ROOT", &f.root)
        .env("CARGO_HOME", f.root.join("unused-cargo-home"))
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains(&format!("jilog: installed {}", f.main)), "{}", stdout(&o));
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(args.contains(&format!("--root {}", f.root.display())), "{args}");
    assert!(args.contains("--locked"), "{args}");

    // Only CARGO_HOME set: the root falls back to it, for install and check.
    let home = f.root.join("cargo-home");
    let o = Command::new("bash")
        .arg(f.checkout.join("scripts/install"))
        .env_remove("CARGO_INSTALL_ROOT")
        .env("CARGO_HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(args.contains(&format!("--root {}", home.display())), "{args}");
    assert!(home.join("bin/jilog").exists());
}

#[test]
fn install_replaces_a_jilog_that_cargo_did_not_install() {
    // azbdmini's jilog was copied in by hand, with no .crates.toml entry, and
    // cargo refused to overwrite it until the script passed --force
    // (jibot-code#q8kf).
    let f = fixture();
    f.fake_jilog("jilog 0.9.0");
    assert!(!f.root.join(".crates.toml").exists());
    let (stubbin, log) = f.stub_cargo();
    let o = Command::new("bash")
        .arg(f.checkout.join("scripts/install"))
        .env("CARGO_INSTALL_ROOT", &f.root)
        .env("CARGO_HOME", f.root.join("unused-cargo-home"))
        .env("PATH", format!("{}:/usr/bin:/bin", stubbin.display()))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains(&format!("jilog: installed {}", f.main)), "{}", stdout(&o));
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(args.contains("--force"), "{args}");
}
