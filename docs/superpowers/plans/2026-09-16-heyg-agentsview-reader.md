# agentsview reader + Spend section — implementation plan (jilog#heyg)

> **STATUS: HISTORICAL RECORD, NOT EXECUTABLE.** This plan was executed on
> 2026-09-16 and the committed code is the source of truth. Two review
> rounds (fresheyes on the whole branch, a Stage 1 reviewer, two delta
> passes) and six roborev patrol reviews changed the implementation after
> the snippets below were written. Where a snippet and the code differ, the
> code wins; the differences are, by file:
> - `readers/claude_code.rs` — canonical-file dedupe runs ONLY when roots can
>   overlap (more than one explicit root, or any profile parent); a legacy
>   single root scans exactly as before, symlinked duplicates included
>   (74803db). A profile parent that cannot be read is skipped with a
>   warning, not an error (bce6668).
> - `readers/agentsview.rs` — `parse_sessions_page`/`parse_messages_page`
>   return `Result`; a 200 without the array, a row without a string `role`,
>   or a chat row without string `content` is an error (bc68692, e78fa5e);
>   pagination uses checked, monotonic ordinals, requires `last_ordinal` on
>   a full page, and errors past MAX_PAGES × PAGE_LIMIT rows instead of
>   returning a partial transcript (bc68692, 74803db, 883f787);
>   `validate_url` accepts only an `http://<ip>[:port]` origin with at most
>   one trailing slash; `new()` re-checks the since_days/timeout bounds as a
>   `Duration`; one `ureq::Agent` per reader with `redirects(0)`; a typed
>   `GetError::Status(code, url)`; cursors and ids percent-encoded
>   (bce6668, bc68692, 5fa7e93, e78fa5e).
> - `archive_spend.rs` — the argv carries `--breakdown` (e38f294); parsing
>   requires `schema_version` 6, integer `microdollars`, and both breakdown
>   arrays — missing or mistyped is an error, never zero (bc68692, 74803db);
>   the failure reason is the lowercased `error:`/`fatal:` line, else the
>   last non-empty line (5fc5a4c, 5fa7e93).
> - `util.rs` — `run_with_timeout` gates `pgid` to unix, prints the
>   `Duration` with `{:?}`, and sits above the tests banner (5fa7e93).
> - `digest.rs` — the agent list suffix is omitted when a period has no
>   per-agent rows (e38f294); `report.archive_spend` is set only when the
>   digest was written or in a dry run (bc68692).
> - `trackers/kata.rs` — issue bodies carry `Agent:`/`Machine:` lines for
>   archive signals (bc68692).
> - `Cargo.toml` — `rust-version = "1.88"` (bc68692).
>
> Original header follows.

> **For agentic workers:** this plan runs under the do-it pipeline (Agency execution, one assign per task, chunk-boundary review). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the nightly loop scans every Claude Code home on this Mac, reads the agentsview archive over HTTP for the sessions raw readers do not cover, and the digest shows what yesterday and the last seven days cost.

**Architecture:** the claude-code reader grows multi-root + profile discovery like the codex reader; the `Reader` trait gains two default methods (`machine`, `dedupe_key`) and signals one optional `machine` field; a new `agentsview` reader talks to the daemon's REST API with `ureq` (no TLS) and a bearer token read at request time; a new `archive_spend` module runs `agentsview usage daily --json` under a hard timeout and folds it into the digest's `## Spend` section.

**Tech Stack:** Rust 2021 (rust-version 1.88 after this change — ureq 2's url/ICU4X chain needs 1.86–1.88; toolchain 1.91.1), serde/serde_json, chrono, rust_decimal, glob, ureq 2 (`default-features = false`), std::process for the CLI fetch. Tests: `cargo test --workspace`, fixtures inline, a loopback `TcpListener` stub for HTTP.

**Spec:** `docs/superpowers/specs/2026-09-16-heyg-agentsview-reader-design.md`

## Global Constraints

- Lens: with no `agentsview` reader and no `paths`/`discover_profiles` on the claude-code reader, every digest line, the frontmatter, and the `review nightly --json` document are byte-identical to today. Existing tests in `digest.rs` and `review.rs` are the guard; do not change their expected strings.
- The nightly never fails because agentsview is down: `discover` errors are warnings (existing behaviour in `run_review`), `usage daily` failures hide the archive block, every HTTP call and the CLI fetch have a hard timeout.
- The bearer token is read from the file at request time, never logged, never in argv, never in a fixture, never in a commit.
- Money is `rust_decimal`; `microdollars` integers → `Decimal::new(m, 6)`. No floats.
- Commit trailer on every commit: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Commit messages in Joi's voice (specific, plain, no idioms).
- Verification per chunk: `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` must pass before the chunk commits.

---

## Chunk 1 — claude-code reader: `paths`, `discover_profiles`, seat tag

### Task 1.1: Multi-root reader with profile discovery

**Files:**
- Modify: `crates/jilog-review/src/readers/claude_code.rs:1-105` (struct, constructors, discover) and the tests at `:160-226`
- Modify: `crates/jilog/src/config.rs:41-44` (`ReaderConfig::ClaudeCode`) and `:301-307` (`into_readers`), tests at `:398+`

**Interfaces:**
- Consumes: `crate::util::expand_tilde`, `glob`, the `Reader` trait as it is today.
- Produces: `ClaudeCodeReader { pub roots: Vec<PathBuf>, pub profile_parents: Vec<PathBuf> }`, `ClaudeCodeReader::new(dir)`, `::from_roots(Vec<PathBuf>)`, `::with_profile_parents(self, Vec<PathBuf>) -> Self`, `::from_default()`, `pub const DEFAULT_PROFILE_PARENTS: [&str; 2] = ["~/.claude-pool/profiles", "~/.claude-profiles"]`, and `Reader::seat` returning the profile directory name for sessions under a profile parent.

- [ ] **Step 1: Write the failing tests** (append to the `tests` module of `claude_code.rs`; keep the two existing tests unchanged)

```rust
    #[test]
    fn paths_and_profiles_are_scanned_and_seat_tagged() {
        let tree = tempfile::tempdir().unwrap();
        let main = tree.path().join(".claude/projects/-Users-joi-x");
        let pool = tree.path().join(".claude-pool/profiles");
        let ctx = tree.path().join(".claude-profiles");
        fs::create_dir_all(&main).unwrap();
        fs::write(main.join("main-session.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        for seat in ["seat-01", "seat-06"] {
            let p = pool.join(seat).join("projects/-Users-joi-y");
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join(format!("{seat}-session.jsonl")), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        }
        // A profile dir without projects/ and a stray file are ignored.
        fs::create_dir_all(pool.join("seat-12")).unwrap();
        fs::write(pool.join("README"), "not a profile").unwrap();
        let glm = ctx.join("glm/projects/-p");
        fs::create_dir_all(&glm).unwrap();
        fs::write(glm.join("glm-session.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();

        let reader = ClaudeCodeReader::from_roots(vec![tree.path().join(".claude/projects")])
            .with_profile_parents(vec![pool.clone(), ctx.clone()]);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        let mut ids: Vec<&str> = handles.iter().map(|h| h.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["glm-session", "main-session", "seat-01-session", "seat-06-session"]);
        for h in &handles {
            let seat = reader.seat(h);
            match h.session_id.as_str() {
                "main-session" => assert_eq!(seat, None, "explicit roots carry no seat"),
                "glm-session" => assert_eq!(seat.as_deref(), Some("glm")),
                "seat-01-session" => assert_eq!(seat.as_deref(), Some("seat-01")),
                "seat-06-session" => assert_eq!(seat.as_deref(), Some("seat-06")),
                other => panic!("unexpected {other}"),
            }
        }
        // A missing profile parent is not an error; duplicate roots scan once.
        let reader = ClaudeCodeReader::from_roots(vec![
            tree.path().join(".claude/projects"),
            tree.path().join(".claude/projects"),
        ])
        .with_profile_parents(vec![tree.path().join("nope")]);
        assert_eq!(reader.discover(since).unwrap().len(), 1);
        // Empty roots scan nothing.
        assert!(ClaudeCodeReader::from_roots(vec![]).discover(since).unwrap().is_empty());
        // An explicit root that is ALSO reachable through a profile parent
        // carries no seat and is scanned once: explicit roots win.
        let reader = ClaudeCodeReader::from_roots(vec![pool.join("seat-06").join("projects")])
            .with_profile_parents(vec![pool.clone()]);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 2, "seat-01 (profile) + seat-06 (explicit), each once");
        for h in &handles {
            match h.session_id.as_str() {
                "seat-06-session" => assert_eq!(reader.seat(h), None, "explicit root wins over the profile parent"),
                "seat-01-session" => assert_eq!(reader.seat(h).as_deref(), Some("seat-01")),
                other => panic!("unexpected {other}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_symlink_root_beats_the_profile_copy() {
        let tree = tempfile::tempdir().unwrap();
        let pool = tree.path().join(".claude-pool/profiles");
        let home = pool.join("seat-06");
        let proj = home.join("projects/-p");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("s.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        // `link` sorts before the pool path? Irrelevant: explicit roots are
        // walked first regardless of lexical order.
        let link = tree.path().join("zzz-link-to-seat-06");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let reader = ClaudeCodeReader::from_roots(vec![link.join("projects")])
            .with_profile_parents(vec![pool.clone()]);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1, "one canonical file, scanned once");
        assert!(handles[0].path.starts_with(link.join("projects")), "the explicit (symlink) path is retained: {}", handles[0].path.display());
        assert_eq!(reader.seat(&handles[0]), None, "explicit root wins, even through a symlink");
    }

    #[test]
    fn default_reader_has_no_profile_parents() {
        let r = ClaudeCodeReader::from_default();
        assert_eq!(r.roots.len(), 1);
        assert!(r.profile_parents.is_empty(), "profile discovery is opt-in");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p jilog-review claude_code -- --nocapture`
Expected: compile error — `from_roots`, `with_profile_parents`, `roots`, `profile_parents` do not exist.

- [ ] **Step 3: Implement the reader**

Replace the struct, constructors and `discover` in `claude_code.rs` (keep `load`, `extract_message` and the module doc, and update the doc's first line to "scans one or more Claude Code project roots"):

```rust
/// Reader for Claude Code session transcripts.
///
/// Glob: `<root>/**/*.jsonl` (recursive) for every root; roots come from
/// `roots` plus, for each `profile_parents` entry, every `<parent>/<name>/projects`
/// directory that exists at scan time (`~/.claude-pool/profiles/seat-NN`,
/// `~/.claude-profiles/glm`, …). Session ID = filename stem of the .jsonl file.
/// Sessions under a profile parent carry `seat = <name>`; explicit roots carry
/// no seat, so the single-root default is unchanged.
pub struct ClaudeCodeReader {
    pub roots: Vec<PathBuf>,
    pub profile_parents: Vec<PathBuf>,
}

/// Profile parents `discover_profiles = true` adds (tilde-expanded at config time).
pub const DEFAULT_PROFILE_PARENTS: [&str; 2] = ["~/.claude-pool/profiles", "~/.claude-profiles"];

impl ClaudeCodeReader {
    /// One explicit root, no profile discovery.
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self::from_roots(vec![projects_dir.into()])
    }

    /// Explicit roots replace the default and do not imply profile discovery.
    pub fn from_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots, profile_parents: Vec::new() }
    }

    /// Add parents whose `<name>/projects` children are scanned as roots.
    pub fn with_profile_parents(mut self, parents: Vec<PathBuf>) -> Self {
        self.profile_parents = parents;
        self
    }

    /// Use the default Claude Code projects directory: `~/.claude/projects`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.claude/projects"))
    }

    /// Explicit roots FIRST (sorted, deduplicated), then the profile roots
    /// discovered under each parent (sorted, deduplicated, minus any that
    /// is also explicit). `discover` walks them in this order and keeps the
    /// first occurrence of each canonical file, so an explicit root always
    /// wins over a profile copy of the same directory — including when the
    /// explicit root is a symlink to a profile home (the retained handle
    /// path is the explicit one, and `seat()` sees it as explicit).
    fn scan_roots(&self) -> Result<Vec<PathBuf>, JilogReviewError> {
        let mut explicit = self.roots.clone();
        explicit.sort();
        explicit.dedup();
        let mut profiles = Vec::new();
        for parent in &self.profile_parents {
            match std::fs::read_dir(parent) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = match entry {
                            Ok(entry) => entry,
                            Err(error) => {
                                tracing::warn!("claude-code: profile entry under {}: {error}", parent.display());
                                continue;
                            }
                        };
                        let projects = entry.path().join("projects");
                        if projects.is_dir() && !explicit.contains(&projects) {
                            profiles.push(projects);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        profiles.sort();
        profiles.dedup();
        explicit.extend(profiles);
        Ok(explicit)
    }
}

impl Reader for ClaudeCodeReader {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for root in self.scan_roots()? {
            if !root.is_dir() {
                continue;
            }
            let pattern = format!("{}/**/*.jsonl", glob::Pattern::escape(&root.to_string_lossy()));
            let entries = match glob::glob(&pattern) {
                Ok(e) => e,
                Err(e) => {
                    return Err(JilogReviewError::Reader(format!("claude-code: glob error: {}", e)));
                }
            };
            for entry in entries.flatten() {
                if entry.is_dir() {
                    continue;
                }
                let canonical = match std::fs::canonicalize(&entry) {
                    Ok(path) => path,
                    Err(error) => {
                        tracing::warn!("claude-code: {}: {error}", entry.display());
                        continue;
                    }
                };
                if !seen.insert(canonical) {
                    continue;
                }
                let session_id = entry
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| entry.display().to_string());
                let modified = match entry.metadata().and_then(|m| m.modified()) {
                    Ok(st) => {
                        let secs = st.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                        Utc.timestamp_opt(secs as i64, 0).single().unwrap_or(Utc::now())
                    }
                    Err(_) => Utc::now(),
                };
                if modified < since {
                    continue;
                }
                handles.push(TranscriptHandle {
                    session_id,
                    path: entry,
                    modified,
                    reader_name: self.name().to_string(),
                    persona: None,
                    channel: None,
                });
            }
        }
        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn seat(&self, handle: &TranscriptHandle) -> Option<String> {
        // Explicit roots win: a session under one carries no seat even when
        // the same directory is reachable through a profile parent.
        if self.roots.iter().any(|root| handle.path.starts_with(root)) {
            return None;
        }
        // `<parent>/<name>/projects/...` → `<name>`.
        self.profile_parents.iter().find_map(|parent| {
            handle
                .path
                .strip_prefix(parent)
                .ok()
                .and_then(|rest| rest.components().next())
                .and_then(|c| c.as_os_str().to_str())
                .map(|s| s.to_string())
        })
    }

    // `load` unchanged.
}
```

The existing `glob` pattern in `discover` did not escape the root; escaping it (as the codex reader does) is a fix, not a behaviour change, for roots without glob metacharacters.

Update `config.rs`:

```rust
    ClaudeCode {
        #[serde(default)]
        path: Option<String>,
        /// Explicit roots; takes precedence over `path`. An empty list scans nothing.
        #[serde(default)]
        paths: Option<Vec<String>>,
        /// Also scan every `~/.claude-pool/profiles/*/projects` and
        /// `~/.claude-profiles/*/projects` present at scan time (opt-in).
        #[serde(default)]
        discover_profiles: bool,
    },
```

and in `into_readers`:

```rust
                    ReaderConfig::ClaudeCode { path, paths, discover_profiles } => {
                        let roots = match (paths, path) {
                            (Some(paths), _) => paths.iter().map(|p| expand_tilde(p)).collect(),
                            (None, Some(path)) => vec![expand_tilde(path)],
                            (None, None) => vec![expand_tilde("~/.claude/projects")],
                        };
                        let mut reader = ClaudeCodeReader::from_roots(roots);
                        if *discover_profiles {
                            reader = reader.with_profile_parents(
                                DEFAULT_PROFILE_PARENTS.iter().map(|p| expand_tilde(p)).collect(),
                            );
                        }
                        Box::new(reader)
                    }
```

Import `DEFAULT_PROFILE_PARENTS` from `jilog_review::readers::claude_code` (add `pub use claude_code::DEFAULT_PROFILE_PARENTS;` in `readers/mod.rs`).

Config test (append to `config.rs` tests):

```rust
    #[test]
    fn claude_code_reader_paths_and_profiles_parse() {
        let cfg = JilogConfig::from_toml_str(
            "[[reader]]\ntype = \"claude-code\"\npaths = [\"/a/projects\", \"/b/projects\"]\ndiscover_profiles = true\n",
        )
        .unwrap();
        match &cfg.readers[0] {
            ReaderConfig::ClaudeCode { paths, discover_profiles, .. } => {
                assert_eq!(paths.as_deref(), Some(&["/a/projects".to_string(), "/b/projects".to_string()][..]));
                assert!(discover_profiles);
            }
            other => panic!("expected claude-code, got {other:?}"),
        }
        assert_eq!(cfg.into_readers()[0].name(), "claude-code");
        // Legacy single path still parses; profiles default off; empty paths scans nothing.
        let cfg = JilogConfig::from_toml_str("[[reader]]\ntype = \"claude-code\"\npath = \"/one\"\n").unwrap();
        assert!(matches!(cfg.readers[0], ReaderConfig::ClaudeCode { discover_profiles: false, .. }));
        let cfg = JilogConfig::from_toml_str("[[reader]]\ntype = \"claude-code\"\npaths = []\n").unwrap();
        assert!(cfg.into_readers()[0].discover(chrono::Utc::now()).unwrap().is_empty());
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/jilog-review/src/readers/claude_code.rs crates/jilog-review/src/readers/mod.rs crates/jilog/src/config.rs
git commit -m "Scan pool and profile roots in the claude-code reader (jilog#heyg)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

**Acceptance (chunk 1):** the four tests above pass; the existing two claude-code tests and every other test pass unchanged; `paths = []` scans nothing; a config with only `path` builds a reader whose `seat()` is always `None`.

---

## Chunk 2 — Reader trait `machine`/`dedupe_key`, `Signal.machine`, run_review dedupe

### Task 2.0: Golden files for the byte-identity lens (written on the UNCHANGED renderer, before any signal change)

**Files:**
- Create: `crates/jilog-review/tests/golden_digest.rs`, `crates/jilog-review/tests/golden/learning-digest.md` (generated)
- Modify: `crates/jilog/src/commands/review.rs` tests (append one test); create `crates/jilog/tests/golden/review-nightly.json` (generated)

**Interfaces:**
- Consumes: today's `render_digest` (11 parameters) and `digest_report_json`; both stay callable through the run (chunk 4 adds one `None` argument at the `render_digest` call site — the golden FILE does not change).
- Produces: two golden files that every later chunk must leave byte-identical.

- [ ] **Step 1: Write the digest golden test**

`crates/jilog-review/tests/golden_digest.rs`:

```rust
//! Byte-identity guard (jilog#heyg lens): a fully populated digest for a
//! configuration WITHOUT the new keys must not change by a single byte.
//! The golden file was generated by this test on the pre-change renderer
//! (`UPDATE_GOLDEN=1 cargo test -p jilog-review --test golden_digest`) and
//! is compared byte-for-byte afterwards. Regenerate ONLY when a change to
//! the digest format is intended and reviewed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::str::FromStr;

use jilog_review::{
    render_digest, signal_title, Correction, DeferralSignal, ErrorSignal, IssueRef, PatternSignal,
    PersonaCounts, Signal, SpendSummary, Workaround,
};
use rust_decimal::Decimal;

const GOLDEN: &str = "tests/golden/learning-digest.md";

fn render_fixture() -> String {
    let c1 = Correction {
        session_id: "842c45ce-77b2-4d72-b995-f2a10466eb40".into(),
        context: "do granola re-auth".into(),
        ..Default::default()
    };
    let c2 = Correction {
        session_id: "chat-1".into(),
        seat: Some("codex-02".into()),
        context: "no, use the gog cli".into(),
        persona: Some("jibot".into()),
        channel: Some("The vibez".into()),
        ..Default::default()
    };
    let e1 = ErrorSignal {
        session_id: "0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2".into(),
        tool_name: "bash".into(),
        message: r#"{"error":null,"output":{"returncode":101,"stderr":"error[E0308]: mismatched types","stdout":""},"success":false}"#.into(),
        ..Default::default()
    };
    let w1 = Workaround {
        session_id: "0000000000000000-79f0e43ee2304cdb_self".into(),
        pattern: "TODO".into(),
        context: "All data collected. Let me update the todo list and finalize".into(),
        ..Default::default()
    };
    let d1 = DeferralSignal {
        session_id: "ae5a0552-47f6-4030-af78-09fe71542d3d".into(),
        item: "next session".into(),
        ..Default::default()
    };
    let p1 = PatternSignal {
        session_id: "ee58d934-1049-4da0-b5b3-9a00f50efcc7".into(),
        seat: Some("seat-03".into()),
        description: "stuck loop".into(),
        pattern_kind: "stuck_loop".into(),
        evidence: "`bash` x6 identical arguments 01:35-01:54".into(),
        ..Default::default()
    };
    let mut p0 = HashMap::new();
    p0.insert(
        "bash".to_string(),
        BTreeSet::from(["s1".to_string(), "s2".to_string(), "s3".to_string()]),
    );
    let d = |s: &str| Decimal::from_str(s).unwrap();
    let spend = SpendSummary {
        total_cost_usd: Some(d("4.2")),
        sessions_with_stats: 3,
        sessions_with_cost: 2,
        input_tokens: 1000,
        output_tokens: 50,
        role_costs: BTreeMap::from([("(root)".to_string(), d("1.2")), ("explore".to_string(), d("3"))]),
        model_costs: BTreeMap::from([("claude-opus-5".to_string(), d("4.2"))]),
    };
    let recurrence = HashMap::from([(signal_title(&Signal::Correction(c1.clone())), "$4.20".to_string())]);
    let issues = HashMap::from([(
        signal_title(&Signal::Workaround(w1.clone())),
        IssueRef { id: "#7".into(), backend: "kata".into(), url: None, title: signal_title(&Signal::Workaround(w1.clone())) },
    )]);
    let personas = BTreeMap::from([(
        "jibot@The vibez".to_string(),
        PersonaCounts {
            persona: "jibot".into(),
            channel: Some("The vibez".into()),
            sessions: 2,
            corrections: 1,
            errors: 0,
            workarounds: 0,
            deferrals: 0,
            patterns: 0,
            input_tokens: 5000,
            output_tokens: 250,
            cost_usd: Some(d("0.5")),
        },
    )]);
    render_digest(
        "2026-09-16",
        &[c1, c2],
        &[e1],
        &[w1],
        &[d1],
        &[p1],
        &p0,
        Some(&spend),
        &recurrence,
        &issues,
        &personas,
    )
}

#[test]
fn digest_bytes_match_golden() {
    let got = render_fixture();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
    }
    let want = std::fs::read_to_string(&path)
        .expect("golden missing — generate it ONCE on the pre-change renderer with UPDATE_GOLDEN=1");
    assert_eq!(got, want, "digest bytes changed for a configuration without the new keys");
}
```

- [ ] **Step 2: Write the JSON golden test** (append to `crates/jilog/src/commands/review.rs` tests)

```rust
    #[test]
    fn review_json_bytes_match_golden() {
        // Byte-identity guard (jilog#heyg lens): the full --json document for
        // a host without agentsview. Generated once on the pre-change code
        // with UPDATE_GOLDEN=1; compared byte-for-byte afterwards.
        let value = digest_report_json(&digest_report(), false);
        let got = serde_json::to_string_pretty(&value).unwrap() + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/review-nightly.json");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &got).unwrap();
        }
        let want = std::fs::read_to_string(&path).expect("golden missing — generate once with UPDATE_GOLDEN=1");
        assert_eq!(got, want, "review JSON bytes changed for a host without agentsview");
    }
```

- [ ] **Step 3: Generate the goldens on the unchanged code, then verify**

Run (the working tree must have NO other change from this plan yet):
```bash
UPDATE_GOLDEN=1 cargo test -p jilog-review --test golden_digest
UPDATE_GOLDEN=1 cargo test -p jilog review_json_bytes_match_golden
cargo test -p jilog-review --test golden_digest && cargo test -p jilog review_json_bytes_match_golden
```
Expected: the two golden files exist; the second run passes without the env var. Read both files once: the digest must contain every section incl. `## Personas` and `## Spend`, the JSON every key of today's document.

- [ ] **Step 4: Commit**

```bash
git add crates/jilog-review/tests/golden_digest.rs crates/jilog-review/tests/golden/learning-digest.md crates/jilog/src/commands/review.rs crates/jilog/tests/golden/review-nightly.json
git commit -m "Pin the digest and review JSON bytes with golden files (jilog#heyg)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

**Acceptance (task 2.0):** both goldens generated on commit `1626391`'s renderer + this test code only; every later chunk runs them unchanged (chunk 4 adds the `None` archive argument at the `render_digest` call in `golden_digest.rs` — the file under `tests/golden/` is never regenerated in this run).

### Task 2.1: Trait methods and the codex dedupe key

**Files:**
- Modify: `crates/jilog-review/src/reader.rs:141-176` (trait), add `trailing_uuid` after `is_sub_agent_session` (`:99`)
- Modify: `crates/jilog-review/src/readers/codex.rs:174-186` (add `dedupe_key` next to `seat`)
- Test: same files

**Interfaces:**
- Produces: `Reader::agent(&self, &TranscriptHandle) -> Option<String>` (default `None`); `Reader::machine(&self, &TranscriptHandle) -> Option<String>` (default `None`); `Reader::dedupe_key(&self, &TranscriptHandle) -> String` (default: the session id); `pub fn trailing_uuid(s: &str) -> Option<&str>` in `reader.rs`.

- [ ] **Step 1: Failing tests**

`reader.rs` tests:

```rust
    #[test]
    fn trailing_uuid_extracts_only_a_well_formed_suffix() {
        assert_eq!(
            trailing_uuid("rollout-2026-03-24T09-02-55-00000000-0000-4000-8000-000000000001"),
            Some("00000000-0000-4000-8000-000000000001")
        );
        assert_eq!(trailing_uuid("00000000-0000-4000-8000-000000000001"), Some("00000000-0000-4000-8000-000000000001"));
        assert_eq!(trailing_uuid("rollout-2026-03-24T09-02-55-test"), None);
        assert_eq!(trailing_uuid("short"), None);
        assert_eq!(trailing_uuid("rollout-x-ZZZZZZZZ-0000-4000-8000-000000000001"), None);
    }
```

`codex.rs` tests (append inside `codex_reader_discovers_dated_rollouts`, after the existing asserts):

```rust
        assert_eq!(
            reader.dedupe_key(&handles[0]),
            "00000000-0000-4000-8000-000000000001",
            "codex dedupe key is the rollout uuid, shared with the archive id"
        );
```

and in `codex_reader_keeps_user_and_assistant_skips_developer` after `assert_eq!(handles.len(), 1);`:

```rust
        assert_eq!(reader.dedupe_key(&handles[0]), handles[0].session_id, "no uuid suffix → the stem itself");
```

- [ ] **Step 2: Run** `cargo test -p jilog-review trailing_uuid && cargo test -p jilog-review codex_reader` — Expected: compile errors (`trailing_uuid`, `dedupe_key` missing).

- [ ] **Step 3: Implement**

`reader.rs`, after `is_sub_agent_session`:

```rust
/// The `8-4-4-4-12` hex uuid `s` ends with, if it ends with one. Used to
/// recognise one Codex session through two readers: the raw rollout stem
/// `rollout-<ts>-<uuid>` and the agentsview archive id `codex:<uuid>`.
pub fn trailing_uuid(s: &str) -> Option<&str> {
    if !s.is_ascii() || s.len() < 36 {
        return None;
    }
    let tail = &s[s.len() - 36..];
    let ok = tail.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    });
    ok.then_some(tail)
}
```

Trait, after `seat`:

```rust
    /// Which agent produced the session (`claude`, `codex`, `cowork`, …),
    /// when the source records it (archive readers). None for local
    /// transcript readers, whose reader name already says which agent.
    fn agent(&self, _handle: &TranscriptHandle) -> Option<String> {
        None
    }

    /// Machine the session ran on, when the source records it (archive
    /// readers). None for local transcript readers.
    fn machine(&self, _handle: &TranscriptHandle) -> Option<String> {
        None
    }

    /// Key under which two readers recognise the same session. The default
    /// is the session id; readers whose ids wrap a shared identifier
    /// (`rollout-<ts>-<uuid>`, `codex:<uuid>`) return that identifier so a
    /// session scanned by a raw reader is not scanned again from the
    /// archive. `run_review` skips a handle whose key was already scanned
    /// this run or is in the processed file, and marks both id and key.
    fn dedupe_key(&self, handle: &TranscriptHandle) -> String {
        handle.session_id.clone()
    }
```

`codex.rs`, after `seat`:

```rust
    fn dedupe_key(&self, handle: &TranscriptHandle) -> String {
        crate::reader::trailing_uuid(&handle.session_id)
            .map(str::to_string)
            .unwrap_or_else(|| handle.session_id.clone())
    }
```

Export `trailing_uuid` from `lib.rs` (`pub use reader::{..., trailing_uuid}`).

- [ ] **Step 4: Run** `cargo test -p jilog-review` — Expected: PASS.

### Task 2.2: `Signal.agent` / `Signal.machine`, stamping, dedupe + alias unmark in `run_review`, digest prefix

**Files:**
- Modify: `crates/jilog-review/src/signal.rs` (all five structs + `Signal::agent()` / `Signal::machine()`)
- Modify: `crates/jilog-review/src/digest.rs:202-448` (`run_review` loop), `:600-616` (unmark block), `:696-800` (`dims_prefix` call sites), `:929-944` (`dims_prefix`)
- Test: `digest.rs` tests module

**Interfaces:**
- Produces: `pub agent: Option<String>` and `pub machine: Option<String>` on `Correction`, `ErrorSignal`, `Workaround`, `PatternSignal`, `DeferralSignal` (each `#[serde(default, skip_serializing_if = "Option::is_none")]`), `Signal::agent(&self) -> Option<&str>`, `Signal::machine(&self) -> Option<&str>`; `dims_prefix(persona, channel, seat, agent, machine)` renders `` `agent:<a>` `` then `` `machine:<m>` `` after the seat span.

- [ ] **Step 1: Failing tests** (append to `digest.rs` tests; `FixtureReader` at `:1396` gains three fields `agent: Option<String>`, `machine: Option<String>`, `dedupe_key: Option<String>` — set to `None` at every existing construction site — and overrides the trait methods)

```rust
        fn agent(&self, _handle: &TranscriptHandle) -> Option<String> {
            self.agent.clone()
        }
        fn machine(&self, _handle: &TranscriptHandle) -> Option<String> {
            self.machine.clone()
        }
        fn dedupe_key(&self, handle: &TranscriptHandle) -> String {
            self.dedupe_key.clone().unwrap_or_else(|| handle.session_id.clone())
        }
```

Tests:

```rust
    #[test]
    fn agent_and_machine_tags_are_stamped_and_rendered() {
        let dir = test_dir("machine-tag");
        let readers: Vec<Box<dyn Reader>> = vec![Box::new(FixtureReader {
            session_id: "a37ffc87-2799-4a09-830b-a92fde71d768".into(),
            messages: correction_messages("no, use the other path"),
            stats: None,
            persona: None,
            channel: None,
            agent: Some("claude".into()),
            machine: Some("macazbd".into()),
            dedupe_key: None,
        })];
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: None,
            date: NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            dry_run: false,
            create_issues: false,
            archive_spend: None,
        };
        let report = run_review(&readers, &crate::trackers::NoneTracker, &args).unwrap();
        assert_eq!(report.corrections[0].agent.as_deref(), Some("claude"));
        assert_eq!(report.corrections[0].machine.as_deref(), Some("macazbd"));
        let body = std::fs::read_to_string(&report.digest_path).unwrap();
        assert!(body.contains("- `agent:claude` `machine:macazbd` `a37ffc87-2799-4a09-830b-a92fde71d768` — 'no, use the other path'"), "{body}");
        // JSON: absent fields are not serialized, present ones are.
        let json = serde_json::to_string(&report.corrections[0]).unwrap();
        assert!(json.contains("\"agent\":\"claude\"") && json.contains("\"machine\":\"macazbd\""));
        let bare = Correction { session_id: "s".into(), context: "c".into(), ..Default::default() };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("agent") && !json.contains("machine"), "{json}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracker_failure_unmarks_the_session_and_its_alias() {
        let dir = test_dir("dedupe-retry");
        let processed = dir.join("processed.txt");
        std::fs::create_dir_all(&dir).unwrap();
        let readers: Vec<Box<dyn Reader>> = vec![Box::new(FixtureReader {
            session_id: "codex:00000000-0000-4000-8000-000000000002".into(),
            messages: correction_messages("no, wrong branch"),
            stats: None, persona: None, channel: None, agent: Some("codex".into()), machine: None,
            dedupe_key: Some("00000000-0000-4000-8000-000000000002".into()),
        })];
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: Some(processed.clone()),
            date: NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            dry_run: false,
            create_issues: true,
            archive_spend: None,
        };
        // OpenTitlesTracker refuses every create → tracker failure → session unmarked.
        let report = run_review(&readers, &OpenTitlesTracker { titles: vec![] }, &args).unwrap();
        assert_eq!(report.tracker_failures, 1);
        let saved = std::fs::read_to_string(&processed).unwrap();
        assert!(!saved.contains("00000000-0000-4000-8000-000000000002"), "neither the id nor the alias may stay processed: {saved}");
        // The retried run scans it again (the sidecar widened the window).
        let report = run_review(&readers, &OpenTitlesTracker { titles: vec![] }, &args).unwrap();
        assert_eq!(report.sessions_scanned, 1, "retry rescans the session");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedupe_key_skips_a_session_seen_through_two_readers() {
        let dir = test_dir("dedupe-key");
        let processed = dir.join("processed.txt");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = FixtureReader {
            session_id: "rollout-2026-09-16T00-00-00-00000000-0000-4000-8000-000000000001".into(),
            messages: correction_messages("no, wrong branch"),
            stats: None, persona: None, channel: None, agent: None, machine: None,
            dedupe_key: Some("00000000-0000-4000-8000-000000000001".into()),
        };
        let archive = FixtureReader {
            session_id: "codex:00000000-0000-4000-8000-000000000001".into(),
            messages: correction_messages("no, wrong branch"),
            stats: None, persona: None, channel: None, agent: Some("codex".into()), machine: Some("macazbd".into()),
            dedupe_key: Some("00000000-0000-4000-8000-000000000001".into()),
        };
        let readers: Vec<Box<dyn Reader>> = vec![Box::new(raw), Box::new(archive)];
        let args = ReviewArgs {
            since: Utc::now() - chrono::Duration::days(1),
            digest_dir: dir.clone(),
            processed_file: Some(processed.clone()),
            date: NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            dry_run: false,
            create_issues: false,
            archive_spend: None,
        };
        let report = run_review(&readers, &crate::trackers::NoneTracker, &args).unwrap();
        assert_eq!(report.sessions_scanned, 1, "the archive copy is skipped in-run");
        assert_eq!(report.corrections.len(), 1);
        assert_eq!(report.corrections[0].machine, None, "the raw reader won");
        let saved = std::fs::read_to_string(&processed).unwrap();
        assert!(saved.contains("00000000-0000-4000-8000-000000000001\n"), "key persisted: {saved}");
        assert!(saved.contains("rollout-2026-09-16T00-00-00-00000000-0000-4000-8000-000000000001\n"));
        // Next run: only the archive reader is configured; the persisted key skips it.
        let archive_only: Vec<Box<dyn Reader>> = vec![Box::new(FixtureReader {
            session_id: "codex:00000000-0000-4000-8000-000000000001".into(),
            messages: correction_messages("no, wrong branch"),
            stats: None, persona: None, channel: None, agent: Some("codex".into()), machine: Some("macazbd".into()),
            dedupe_key: Some("00000000-0000-4000-8000-000000000001".into()),
        })];
        let report = run_review(&archive_only, &crate::trackers::NoneTracker, &args).unwrap();
        assert_eq!(report.sessions_scanned, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

(`archive_spend: None` in `ReviewArgs` is introduced in chunk 4; until then omit that line — chunk 4's task updates every `ReviewArgs` literal.)

- [ ] **Step 2: Run** `cargo test -p jilog-review digest` — Expected: compile errors (`machine` field, `dedupe_key`).

- [ ] **Step 3: Implement**

`signal.rs` — add to each of the five structs, directly after `seat`:

```rust
    /// Which agent produced the session, when the reader knows it (archive
    /// readers stamp agentsview's `agent`). Absent for local transcript
    /// readers, so their JSON is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Machine the session ran on, when the reader knows it (archive
    /// readers stamp the agentsview machine label). Absent for local
    /// transcript readers, so their JSON is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
```

and on `Signal`:

```rust
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::Correction(s) => s.agent.as_deref(),
            Self::Error(s) => s.agent.as_deref(),
            Self::Workaround(s) => s.agent.as_deref(),
            Self::Pattern(s) => s.agent.as_deref(),
            Self::Deferral(s) => s.agent.as_deref(),
        }
    }

    pub fn machine(&self) -> Option<&str> {
        match self {
            Self::Correction(s) => s.machine.as_deref(),
            Self::Error(s) => s.machine.as_deref(),
            Self::Workaround(s) => s.machine.as_deref(),
            Self::Pattern(s) => s.machine.as_deref(),
            Self::Deferral(s) => s.machine.as_deref(),
        }
    }
```

`digest.rs` `run_review`: declare `let mut seen_keys: HashSet<String> = HashSet::new();` and `let mut aliases: HashMap<String, String> = HashMap::new();` (session id → dedupe key, only when they differ) next to `scanned_modified`. In the handle loop, replace the processed check:

```rust
            // One session, two readers (raw transcript + archive): the first
            // reader in config order wins; the later copy is skipped by its
            // dedupe key, in-run and across runs via the processed file.
            let key = reader.dedupe_key(&handle);
            if seen_keys.contains(&key) {
                continue;
            }
            if let Some(ref ps) = processed {
                if ps.contains(&handle.session_id) || ps.contains(&key) {
                    continue;
                }
            }
```

Both places that `ps.mark(&handle.session_id)` become:

```rust
                        seen_keys.insert(key.clone());
                        if key != handle.session_id {
                            aliases.insert(handle.session_id.clone(), key.clone());
                        }
                        if let Some(ref mut ps) = processed {
                            ps.mark(&handle.session_id);
                            if key != handle.session_id {
                                ps.mark(&key);
                            }
                        }
```

(`seen_keys.insert` runs whether or not a processed file is configured.) The unmark block near the end of `run_review` (`for sid in &failed_sessions { ps.unmark(sid); }` and the `unresolved_pending` loop) unmarks the alias as well:

```rust
            for sid in failed_sessions.iter().chain(unresolved_pending.iter()) {
                ps.unmark(sid);
                if let Some(alias) = aliases.get(sid) {
                    ps.unmark(alias);
                }
            }
```

After the seat stamping block add the same five loops for `let agent = reader.agent(&handle);` → `signal.agent.clone_from(&agent);` and `let machine = reader.machine(&handle);` → `signal.machine.clone_from(&machine);`.

`dims_prefix`:

```rust
fn dims_prefix(
    persona: &Option<String>,
    channel: &Option<String>,
    seat: &Option<String>,
    agent: &Option<String>,
    machine: &Option<String>,
) -> String {
    let mut prefix = match persona_key(persona, channel) {
        Some(key) => format!("`{}` ", key),
        None => String::new(),
    };
    if let Some(seat) = seat {
        prefix.push_str(&format!("`seat:{}` ", sanitize_display(seat)));
    }
    if let Some(agent) = agent {
        prefix.push_str(&format!("`agent:{}` ", sanitize_display(agent)));
    }
    if let Some(machine) = machine {
        prefix.push_str(&format!("`machine:{}` ", sanitize_display(machine)));
    }
    prefix
}
```

Every `dims_prefix(&x.persona, &x.channel, &x.seat)` call gains `, &x.agent, &x.machine`; the test `seat_prefix_preserves_fleet_dimensions_and_sanitizes` (`:2302`) passes `&None, &None` as the fourth and fifth arguments. The `test_dir` helper in `digest.rs` tests must exist (it does: `fn test_dir`). Other `FixtureReader` literals in the file get `agent: None, machine: None, dedupe_key: None`.

Exhaustive struct literals: 19 existing literals of the five signal structs list every field without `..Default::default()` and stop compiling once the two fields exist — 6 `Correction {`, 4 `ErrorSignal {`, 1 `Workaround {`, 6 `PatternSignal {`, 2 `DeferralSignal {` across `detectors.rs`, `digest.rs` (e.g. `:1164`, `:1469`, `:1631`, `:1737`), `health.rs`, `signal.rs`, `tracker.rs`, `trackers/kata.rs`, `trackers/none.rs`. Find them with `grep -rn 'Correction {\|ErrorSignal {\|Workaround {\|PatternSignal {\|DeferralSignal {' crates | grep -v 'pub struct\|Signal::\|Default::default'` and add `agent: None, machine: None,` right after each `seat:` line (production code in `detectors.rs`/`health.rs` constructs signals with explicit `seat: None` — keep the explicit style there; tests may switch to `..Default::default()`). The compiler is the checklist: `cargo build --workspace --tests` must be clean before running tests.

- [ ] **Step 4: Run** `cargo test --workspace && cargo clippy --workspace -- -D warnings` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jilog-review/src/reader.rs crates/jilog-review/src/lib.rs crates/jilog-review/src/readers/codex.rs crates/jilog-review/src/signal.rs crates/jilog-review/src/digest.rs
git commit -m "Tag signals with agent and machine; dedupe sessions across readers (jilog#heyg)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

**Acceptance (chunk 2):** the three new tests pass; existing digest tests are unchanged and pass (signals without a machine render exactly as before); the processed file holds both the session id and the key for a codex session.

---

## Chunk 3 — agentsview reader over HTTP

### Task 3.1: Dependency, client, parsers

**Files:**
- Modify: `Cargo.toml` (workspace deps: `ureq = { version = "2", default-features = false }`), `crates/jilog-review/Cargo.toml` (`ureq = { workspace = true }`)
- Create: `crates/jilog-review/src/readers/agentsview.rs`
- Modify: `crates/jilog-review/src/readers/mod.rs` (`pub mod agentsview; pub use agentsview::AgentsviewReader;`)

**Interfaces:**
- Produces:
  - `pub const DEFAULT_URL: &str = "http://127.0.0.1:8080"`, `DEFAULT_TOKEN_FILE: &str = "~/.agentsview/config.toml"`, `DEFAULT_SINCE_DAYS: u32 = 7`, `DEFAULT_TIMEOUT_SECS: u64 = 30`
  - `pub fn validate_url(url: &str) -> Result<String, JilogReviewError>` (trims a trailing `/`; rejects anything not starting with `http://`) — used by config parsing AND the constructor
  - `pub struct AgentsviewReader` with `pub fn new(url: &str, token_file: PathBuf, since_days: u32, timeout: Duration) -> Result<Self, JilogReviewError>` (calls `validate_url`), `pub fn probe(&self) -> Result<(), JilogReviewError>` (`GET /api/v1/machines`)
  - `pub fn read_token(path: &Path) -> Result<String, JilogReviewError>` — TOML table → `auth_token` string required (missing → error); not TOML → trimmed body; empty → error
  - `pub struct ArchiveSession { pub id: String, pub agent: String, pub machine: String, pub started_at: Option<DateTime<Utc>>, pub ended_at: Option<DateTime<Utc>> }`
  - `pub fn parse_sessions_page(v: &serde_json::Value) -> (Vec<ArchiveSession>, Option<String>)`
  - `pub fn parse_messages_page(v: &serde_json::Value) -> (Vec<Message>, usize, Option<i64>)` (messages, `count`, `last_ordinal`)
  - `pub fn parse_usage(v: &serde_json::Value) -> Option<SessionStats>`
  - `pub fn parse_machines(v: &serde_json::Value) -> HashMap<String, String>`
  - `pub fn strip_agent_prefix(id: &str) -> &str`
  - `pub fn microdollars_to_usd(m: i64) -> Decimal`

- [ ] **Step 1: Failing tests** (the `tests` module of `agentsview.rs`; fixtures are trimmed copies of the live shapes recorded in the spec)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SESSIONS_PAGE_1: &str = r#"{"sessions":[
      {"id":"codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373","project":"i_wa","machine":"4a70f2a82b531088b36678f1b96a47f8","agent":"codex","started_at":"2026-09-15T21:52:03.487Z","ended_at":"2026-09-16T00:00:19.938Z","is_automated":false},
      {"id":"a37ffc87-2799-4a09-830b-a92fde71d768","project":"jibot","machine":"4a70f2a82b531088b36678f1b96a47f8","agent":"claude","started_at":"2026-09-11T21:57:31.589Z","ended_at":"2026-09-11T23:10:00Z","is_automated":true}
    ],"next_cursor":"CURSOR1","total":3}"#;
    const SESSIONS_PAGE_2: &str = r#"{"sessions":[
      {"id":"cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07","project":"x","machine":"deadbeef","agent":"cowork","started_at":"2026-09-15T01:00:00Z","ended_at":null}
    ],"next_cursor":null,"total":3}"#;
    const MESSAGES_PAGE: &str = r#"{"count":4,"first_ordinal":0,"last_ordinal":3,"messages":[
      {"ordinal":0,"role":"user","content":"clean up the worktree","timestamp":"2026-09-11T21:57:31.589Z","model":""},
      {"ordinal":1,"role":"assistant","content":"On it.","timestamp":"2026-09-11T21:57:35.717Z","model":"claude-fable-5-1"},
      {"ordinal":2,"content":"no role here"},
      {"ordinal":3,"role":"tool","content":"{\"success\":false}"}
    ]}"#;
    const USAGE: &str = r#"{"session_id":"a37ffc87-2799-4a09-830b-a92fde71d768","agent":"claude","total_output_tokens":360,"peak_context_tokens":326390,"has_token_data":true,"cost":{"microdollars":717783},"has_cost":true,"cost_usd":0.717783,"models":["claude-fable-5-1"],"unpriced_models":[],"breakdown_count":2,"breakdown":[
      {"ordinal":1,"model":"claude-fable-5-1","input_tokens":2,"output_tokens":174,"cache_creation_input_tokens":29284,"cache_read_input_tokens":34443,"cost":{"microdollars":603011},"has_cost":true},
      {"ordinal":2,"model":"claude-fable-5-1","input_tokens":30,"output_tokens":186,"cache_creation_input_tokens":4476,"cache_read_input_tokens":63727,"cost":{"microdollars":114772},"has_cost":true}
    ]}"#;
    const USAGE_UNPRICED: &str = r#"{"session_id":"x","agent":"cowork","total_output_tokens":10,"has_token_data":true,"cost":{"microdollars":0},"has_cost":false,"models":["mystery"],"unpriced_models":["mystery"],"breakdown_count":1,"breakdown":[{"ordinal":1,"model":"mystery","input_tokens":5,"output_tokens":10,"cost":{"microdollars":0},"has_cost":false}]}"#;
    const USAGE_EMPTY: &str = r#"{"session_id":"x","agent":"codex","total_output_tokens":0,"has_token_data":false,"cost":{"microdollars":0},"has_cost":false,"models":[],"breakdown_count":0,"breakdown":[]}"#;
    const MACHINES: &str = r#"{"machines":["4a70f2a82b531088b36678f1b96a47f8"],"machine_labels":{"4a70f2a82b531088b36678f1b96a47f8":"macazbd"},"machine_aliases":{"local":"4a70f2a82b531088b36678f1b96a47f8"}}"#;

    #[test]
    fn parses_sessions_messages_usage_and_machines() {
        let (page, cursor) = parse_sessions_page(&serde_json::from_str(SESSIONS_PAGE_1).unwrap());
        assert_eq!(cursor.as_deref(), Some("CURSOR1"));
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].agent, "codex");
        assert!(page[0].ended_at.is_some());
        let (page2, cursor2) = parse_sessions_page(&serde_json::from_str(SESSIONS_PAGE_2).unwrap());
        assert_eq!(cursor2, None);
        assert_eq!(page2[0].ended_at, None, "null ended_at is None");

        let (msgs, count, last) = parse_messages_page(&serde_json::from_str(MESSAGES_PAGE).unwrap());
        assert_eq!((count, last), (4, Some(3)));
        assert_eq!(msgs.len(), 2, "role-less and tool rows are dropped: no tool identity, so no error signals");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[1].content.as_ref().and_then(|c| c.as_str()), Some("On it."));

        let stats = parse_usage(&serde_json::from_str(USAGE).unwrap()).expect("priced session");
        assert_eq!(stats.cost_usd.as_deref(), Some("0.717783"));
        assert_eq!(stats.input_tokens, 32);
        assert_eq!(stats.output_tokens, 360);
        assert_eq!(stats.model_costs.get("claude-fable-5-1").map(String::as_str), Some("0.717783"));
        let unpriced = parse_usage(&serde_json::from_str(USAGE_UNPRICED).unwrap()).expect("tokens without cost");
        assert_eq!(unpriced.cost_usd, None);
        assert_eq!(unpriced.input_tokens, 5);
        assert!(unpriced.model_costs.is_empty());
        assert_eq!(parse_usage(&serde_json::from_str(USAGE_EMPTY).unwrap()), None);

        let labels = parse_machines(&serde_json::from_str(MACHINES).unwrap());
        assert_eq!(labels.get("4a70f2a82b531088b36678f1b96a47f8").map(String::as_str), Some("macazbd"));

        assert_eq!(strip_agent_prefix("codex:abc"), "abc");
        assert_eq!(strip_agent_prefix("a37ffc87-2799-4a09-830b-a92fde71d768"), "a37ffc87-2799-4a09-830b-a92fde71d768");
        assert_eq!(strip_agent_prefix("weird:"), "weird:");
        assert_eq!(microdollars_to_usd(52872492).to_string(), "52.872492");
        assert_eq!(microdollars_to_usd(0).to_string(), "0.000000");
    }

    #[test]
    fn read_token_accepts_toml_and_bare_files_and_rejects_empty_or_tokenless_toml() {
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("config.toml");
        std::fs::write(&toml, "require_auth = true\nauth_token = \"tok-123\"\n[agents]\n").unwrap();
        assert_eq!(read_token(&toml).unwrap(), "tok-123");
        let bare = dir.path().join("token");
        std::fs::write(&bare, "  tok-456\n").unwrap();
        assert_eq!(read_token(&bare).unwrap(), "tok-456");
        // A TOML file WITHOUT auth_token is a config file, never a token:
        // its body must not be sent anywhere.
        let other = dir.path().join("other.toml");
        std::fs::write(&other, "cursor_secret = \"sec-789\"\n").unwrap();
        let err = read_token(&other).unwrap_err().to_string();
        assert!(err.contains("no auth_token"), "{err}");
        assert!(!err.contains("sec-789"), "errors never echo file contents: {err}");
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "\n").unwrap();
        let err = read_token(&empty).unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
        assert!(!err.contains("tok-"), "errors never echo token material");
        assert!(read_token(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn validate_url_accepts_only_http_ip_origins() {
        assert_eq!(validate_url("http://127.0.0.1:8080/").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(validate_url("http://100.64.0.9:8080").unwrap(), "http://100.64.0.9:8080");
        assert_eq!(validate_url("http://[::1]:8080").unwrap(), "http://[::1]:8080");
        assert_eq!(validate_url("http://127.0.0.1").unwrap(), "http://127.0.0.1");
        for bad in [
            "https://127.0.0.1:8080",      // no TLS
            "http://localhost:8080",       // hostname → DNS → unbounded
            "http://collector:8080",
            "http://127.0.0.1:8080/api/v1", // path
            "http://127.0.0.1:8080?x=1",
            "http://127.0.0.1:8080#f",
            "http://user@127.0.0.1:8080",
            "http://127.0.0.1:99999",
            "http://[::1",
            "http://",
            "127.0.0.1:8080",
        ] {
            let err = validate_url(bad).unwrap_err().to_string();
            assert!(err.contains("http://<ip>[:port]"), "{bad}: {err}");
        }
        assert!(AgentsviewReader::new("https://x", PathBuf::from("/nonexistent"), 7, Duration::from_secs(1)).is_err());
        assert!(AgentsviewReader::new("http://127.0.0.1:8080/", PathBuf::from("/x"), 7, Duration::from_secs(1)).is_ok());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p jilog-review agentsview` — Expected: compile errors.

- [ ] **Step 3: Implement the module**

```rust
//! AgentsviewReader — sessions, messages and usage from an agentsview
//! daemon (kenn-io/agentsview) over its REST API.
//!
//! The archive covers every agent agentsview syncs (Claude Code pool seats
//! and profiles, Codex pool, cowork, cursor, copilot, hermes, pi, …) and
//! every machine it collects from. This reader turns each archived session
//! into a [`TranscriptHandle`] whose id is the archive id (`<agent>:<id>`;
//! Claude sessions are the bare transcript uuid), loads its user/assistant
//! rows as Schema-B messages, reports per-session usage as
//! [`SessionStats`], and tags signals with the machine label.
//!
//! Dedupe: [`Reader::dedupe_key`] strips the agent prefix, so a session a
//! raw reader already scanned (same uuid) is skipped by `run_review`. List
//! this reader AFTER the raw readers in `jilog.toml`.
//!
//! Transport: plain HTTP (`ureq`, no TLS) — the daemon is loopback or
//! tailnet-HTTP. `Authorization: Bearer <token>`; the token is read from
//! `token_file` (a TOML with `auth_token`, or a bare token) at request
//! time and never logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, SessionStats, TranscriptHandle};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_TOKEN_FILE: &str = "~/.agentsview/config.toml";
pub const DEFAULT_SINCE_DAYS: u32 = 7;
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Sessions per page (the daemon's maximum).
const PAGE_LIMIT: usize = 500;
/// Pagination ceiling per discover (500 × 40 = 20k sessions).
const MAX_PAGES: usize = 40;

/// One archived session as listed by `/api/v1/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSession {
    pub id: String,
    pub agent: String,
    pub machine: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

pub struct AgentsviewReader {
    base_url: String,
    token_file: PathBuf,
    since_days: u32,
    timeout: Duration,
    /// id → (agent, machine key), filled by `discover`.
    sessions: Mutex<HashMap<String, (String, String)>>,
    /// machine key → label, filled by `discover`.
    labels: Mutex<HashMap<String, String>>,
}

/// The daemon ORIGIN, normalized: `http://<ip>[:port]` with an IP-literal
/// host (v4, or bracketed v6) and nothing after the authority (a single
/// trailing `/` is tolerated and trimmed). Refused: any other scheme (no
/// TLS support), a hostname (ureq cannot bound DNS resolution by the
/// timeout — the nightly could hang on a dead resolver), userinfo, a path,
/// a query, a fragment, or a bad port. Called by `JilogConfig::from_toml_str`
/// so a bad url fails at config load, and again by `AgentsviewReader::new`.
pub fn validate_url(url: &str) -> Result<String, JilogReviewError> {
    let bad = |why: &str| {
        JilogReviewError::Reader(format!(
            "agentsview: url must be http://<ip>[:port] ({}; no TLS, no DNS): {}",
            why, url
        ))
    };
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(|| bad("scheme must be http://"))?
        .trim_end_matches('/');
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return Err(bad("origin only — no path, query, fragment or userinfo"));
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (h, after) = rest.split_once(']').ok_or_else(|| bad("unterminated IPv6 literal"))?;
            (h.to_string(), after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        },
    };
    host.parse::<std::net::IpAddr>().map_err(|_| bad("host must be an IP literal"))?;
    if let Some(p) = port {
        p.parse::<u16>().map_err(|_| bad("port must be 0-65535"))?;
    }
    Ok(format!("http://{}", authority))
}

impl AgentsviewReader {
    pub fn new(url: &str, token_file: PathBuf, since_days: u32, timeout: Duration) -> Result<Self, JilogReviewError> {
        let base_url = validate_url(url)?;
        Ok(Self {
            base_url,
            token_file,
            since_days,
            timeout,
            sessions: Mutex::new(HashMap::new()),
            labels: Mutex::new(HashMap::new()),
        })
    }

    /// GET `<base>/api/v1<path_and_query>` with the bearer token, parsed as JSON.
    fn get(&self, path_and_query: &str) -> Result<serde_json::Value, JilogReviewError> {
        let token = read_token(&self.token_file)?;
        let url = format!("{}/api/v1{}", self.base_url, path_and_query);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.timeout)
            .timeout(self.timeout)
            .build();
        let resp = match agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", token))
            .set("Accept", "application/json")
            .call()
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) => {
                return Err(JilogReviewError::Reader(format!("agentsview: {} → HTTP {}", url, code)));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(JilogReviewError::Reader(format!("agentsview: {} unreachable: {}", url, t.kind())));
            }
        };
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut body)
            .map_err(|e| JilogReviewError::Reader(format!("agentsview: {} read: {}", url, e)))?;
        serde_json::from_slice(&body)
            .map_err(|e| JilogReviewError::Reader(format!("agentsview: {} bad JSON: {}", url, e)))
    }

    fn get_ok(&self, path_and_query: &str) -> Result<serde_json::Value, JilogReviewError> {
        self.get(path_and_query)
    }

    /// Bounded reachability check (`GET /api/v1/machines`). The CLI runs
    /// it before the `usage daily` spend fetch, which would otherwise
    /// answer from a stale local archive while the daemon is down.
    pub fn probe(&self) -> Result<(), JilogReviewError> {
        self.get("/machines").map(|_| ())
    }
}

/// Read the bearer token. A body that parses as a TOML table is a config
/// file and MUST carry a string `auth_token` — a table without it is an
/// error, never a fallback to the body (another setting's value must not
/// be sent as a bearer). A body that is not TOML is a bare token, trimmed.
/// Errors name the path, never the contents.
pub fn read_token(path: &Path) -> Result<String, JilogReviewError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| JilogReviewError::Reader(format!("agentsview: token file {}: {}", path.display(), e)))?;
    let token = match raw.parse::<toml::Table>() {
        Ok(table) if !table.is_empty() => match table.get("auth_token").and_then(|v| v.as_str()) {
            Some(t) => t.trim().to_string(),
            None => {
                return Err(JilogReviewError::Reader(format!(
                    "agentsview: token file {} is a TOML config with no auth_token",
                    path.display()
                )))
            }
        },
        _ => raw.trim().to_string(),
    };
    if token.is_empty() {
        return Err(JilogReviewError::Reader(format!(
            "agentsview: token file {} is empty (expected auth_token = \"…\" or a bare token)",
            path.display()
        )));
    }
    Ok(token)
}

/// `<agent>:<id>` → `<id>`; ids without a prefix (Claude) are returned as is.
pub fn strip_agent_prefix(id: &str) -> &str {
    match id.split_once(':') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => id,
    }
}

pub fn microdollars_to_usd(m: i64) -> Decimal {
    Decimal::new(m, 6)
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn time_field(v: &serde_json::Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub fn parse_sessions_page(v: &serde_json::Value) -> (Vec<ArchiveSession>, Option<String>) {
    let sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(ArchiveSession {
                        id: str_field(row, "id")?,
                        agent: str_field(row, "agent").unwrap_or_default(),
                        machine: str_field(row, "machine").unwrap_or_default(),
                        started_at: time_field(row, "started_at"),
                        ended_at: time_field(row, "ended_at"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (sessions, str_field(v, "next_cursor").filter(|c| !c.is_empty()))
}

/// Only `user` and `assistant` rows become messages. The archive's rows
/// carry no tool name (tool identity lives under `/tool-calls`), so a
/// `tool` row would reach `detect_errors` as tool "unknown" and bypass the
/// expected-noise rules — archive sessions produce no error signals.
pub fn parse_messages_page(v: &serde_json::Value) -> (Vec<Message>, usize, Option<i64>) {
    let messages = v
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let role = str_field(row, "role")?;
                    if role != "user" && role != "assistant" {
                        return None;
                    }
                    let content = row.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                    Some(Message { role: Some(role), content: Some(serde_json::Value::String(content)), name: None })
                })
                .collect()
        })
        .unwrap_or_default();
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(messages.len() as u64) as usize;
    let last = v.get("last_ordinal").and_then(|c| c.as_i64());
    (messages, count, last)
}

/// `None` when the archive has neither token data nor a cost for the session.
pub fn parse_usage(v: &serde_json::Value) -> Option<SessionStats> {
    let has_tokens = v.get("has_token_data").and_then(|b| b.as_bool()).unwrap_or(false);
    let has_cost = v.get("has_cost").and_then(|b| b.as_bool()).unwrap_or(false);
    if !has_tokens && !has_cost {
        return None;
    }
    let micro = |x: &serde_json::Value| x.get("cost").and_then(|c| c.get("microdollars")).and_then(|m| m.as_i64());
    let mut stats = SessionStats {
        cost_usd: if has_cost { micro(v).map(|m| microdollars_to_usd(m).to_string()) } else { None },
        input_tokens: 0,
        output_tokens: v.get("total_output_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
        role: None,
        model_costs: Default::default(),
    };
    let mut model_costs: std::collections::BTreeMap<String, Decimal> = Default::default();
    for row in v.get("breakdown").and_then(|b| b.as_array()).into_iter().flatten() {
        stats.input_tokens += row.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
        let priced = row.get("has_cost").and_then(|b| b.as_bool()).unwrap_or(false);
        if let (true, Some(model), Some(m)) = (priced, str_field(row, "model"), micro(row)) {
            *model_costs.entry(model).or_insert(Decimal::ZERO) += microdollars_to_usd(m);
        }
    }
    stats.model_costs = model_costs.into_iter().map(|(k, d)| (k, d.to_string())).collect();
    Some(stats)
}

pub fn parse_machines(v: &serde_json::Value) -> HashMap<String, String> {
    v.get("machine_labels")
        .and_then(|m| m.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

impl Reader for AgentsviewReader {
    fn name(&self) -> &str {
        "agentsview"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let floor = Utc::now() - chrono::Duration::days(self.since_days as i64);
        let since_eff = std::cmp::max(since, floor);
        match self.get_ok("/machines") {
            Ok(v) => *self.labels.lock().unwrap() = parse_machines(&v),
            Err(e) => tracing::warn!("agentsview: machine labels unavailable, using keys: {}", e),
        }
        let mut handles = Vec::new();
        let mut cursor: Option<String> = None;
        let mut index = self.sessions.lock().unwrap();
        for _ in 0..MAX_PAGES {
            let mut query = format!(
                "/sessions?limit={}&active_since={}&include_automated=true&include_one_shot=true&include_children=true",
                PAGE_LIMIT,
                since_eff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            );
            if let Some(c) = &cursor {
                query.push_str("&cursor=");
                query.push_str(c);
            }
            let (page, next) = parse_sessions_page(&self.get_ok(&query)?);
            for s in page {
                // A session without `ended_at` is still active: it was
                // modified "now", whatever its `started_at` says — a
                // long-running session that started before `since` but
                // had activity inside the window must not be dropped.
                let modified = s.ended_at.unwrap_or_else(Utc::now);
                if modified < since {
                    continue;
                }
                index.insert(s.id.clone(), (s.agent.clone(), s.machine.clone()));
                handles.push(TranscriptHandle {
                    session_id: s.id.clone(),
                    path: PathBuf::from(format!("agentsview://{}", s.id)),
                    modified,
                    reader_name: self.name().to_string(),
                    persona: None,
                    channel: None,
                });
            }
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        let mut out = Vec::new();
        let mut from: i64 = 0;
        for _ in 0..MAX_PAGES {
            let v = self.get_ok(&format!(
                "/sessions/{}/messages?from={}&limit={}&direction=asc",
                handle.session_id, from, PAGE_LIMIT
            ))?;
            let (msgs, count, last) = parse_messages_page(&v);
            out.extend(msgs);
            match last {
                Some(l) if count >= PAGE_LIMIT => from = l + 1,
                _ => break,
            }
        }
        Ok(out)
    }

    fn load_stats(&self, handle: &TranscriptHandle) -> Result<Option<SessionStats>, JilogReviewError> {
        match self.get(&format!("/sessions/{}/usage?breakdown=true", handle.session_id)) {
            Ok(v) => Ok(parse_usage(&v)),
            Err(JilogReviewError::Reader(msg)) if msg.contains("HTTP 404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn agent(&self, handle: &TranscriptHandle) -> Option<String> {
        let agent = self.sessions.lock().unwrap().get(&handle.session_id)?.0.clone();
        (!agent.is_empty()).then_some(agent)
    }

    fn machine(&self, handle: &TranscriptHandle) -> Option<String> {
        let key = self.sessions.lock().unwrap().get(&handle.session_id)?.1.clone();
        if key.is_empty() {
            return None;
        }
        Some(self.labels.lock().unwrap().get(&key).cloned().unwrap_or(key))
    }

    fn dedupe_key(&self, handle: &TranscriptHandle) -> String {
        strip_agent_prefix(&handle.session_id).to_string()
    }
}
```

(`get_ok` exists only so `load_stats` can distinguish a 404 without re-matching; if the implementer prefers, inline it.) Session ids are plain uuids or `<agent>:<uuid>`; they go into the URL path unescaped (the daemon's own CLI does the same). `toml` is already a dependency of `jilog-review`.

- [ ] **Step 4: Run** `cargo test -p jilog-review agentsview && cargo clippy --workspace -- -D warnings` — Expected: PASS.

### Task 3.2: Loopback stub server test + config wiring

**Files:**
- Modify: `crates/jilog-review/src/readers/agentsview.rs` tests
- Modify: `crates/jilog/src/config.rs` (`ReaderConfig::Agentsview`, `into_readers`, tests)

**Interfaces:**
- Produces: `ReaderConfig::Agentsview { url: Option<String>, token_file: Option<String>, since_days: Option<u32>, timeout_secs: Option<u64>, bin: Option<String> }`; `from_toml_str` rejects a non-`http://` url; `pub struct AgentsviewSettings { pub url: String, pub token_file: PathBuf, pub since_days: u32, pub timeout: Duration, pub bin: PathBuf }` and `JilogConfig::agentsview_settings(&self) -> Option<AgentsviewSettings>` (resolved defaults from the first agentsview reader; `None` when none is configured).

- [ ] **Step 1: Failing tests**

Stub server helper and tests in `agentsview.rs`:

```rust
    /// Minimal HTTP/1.1 stub: serves canned JSON by path prefix, records
    /// the request line + Authorization header of every request.
    fn stub_server(routes: Vec<(&'static str, String)>) -> (String, std::sync::Arc<Mutex<Vec<(String, String)>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream { Ok(s) => s, Err(_) => break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request = line.trim().to_string();
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h == "\r\n" { break; }
                    if let Some(v) = h.strip_prefix("Authorization: ") { auth = v.trim().to_string(); }
                }
                let path = request.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push((path.clone(), auth));
                let body = routes.iter().find(|(p, _)| path.contains(p)).map(|(_, b)| b.clone());
                let (status, body) = match body { Some(b) => ("200 OK", b), None => ("404 Not Found", "{}".to_string()) };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status, body.len(), body
                );
            }
        });
        (base, seen)
    }

```

Routing is `path.contains(p)` with the FIRST matching route winning, so the more specific `cursor=CURSOR1` route is listed first:

```rust
    #[test]
    fn reader_pages_sessions_tags_machines_and_sends_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("config.toml");
        std::fs::write(&token_file, "auth_token = \"secret-1\"\n").unwrap();
        let (base, seen) = stub_server(vec![
            ("cursor=CURSOR1", SESSIONS_PAGE_2.to_string()),
            ("/api/v1/sessions?limit=500", SESSIONS_PAGE_1.to_string()),
            ("/api/v1/machines", MACHINES.to_string()),
            ("a37ffc87-2799-4a09-830b-a92fde71d768/messages", MESSAGES_PAGE.to_string()),
            ("a37ffc87-2799-4a09-830b-a92fde71d768/usage", USAGE.to_string()),
            ("cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07/usage", USAGE_EMPTY.to_string()),
        ]);
        let reader = AgentsviewReader::new(&base, token_file, 3650, Duration::from_secs(5)).unwrap();
        let since = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z").unwrap().with_timezone(&Utc);
        let handles = reader.discover(since).unwrap();
        let mut ids: Vec<&str> = handles.iter().map(|h| h.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, [
            "a37ffc87-2799-4a09-830b-a92fde71d768",
            "codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373",
            "cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07",
        ], "both pages, automated included");
        let claude = handles.iter().find(|h| h.session_id.starts_with("a37f")).unwrap();
        let cowork = handles.iter().find(|h| h.session_id.starts_with("cowork")).unwrap();
        assert_eq!(reader.agent(claude).as_deref(), Some("claude"));
        assert_eq!(reader.agent(cowork).as_deref(), Some("cowork"));
        assert_eq!(reader.machine(claude).as_deref(), Some("macazbd"), "label from /machines");
        assert_eq!(reader.machine(cowork).as_deref(), Some("deadbeef"), "unknown key falls back to the key");
        assert!(reader.probe().is_ok());
        assert_eq!(reader.dedupe_key(cowork), "53c9fd03-e50a-4baa-aadd-cb1c75c60f07");
        assert_eq!(reader.dedupe_key(claude), claude.session_id);
        assert_eq!(reader.load(claude).unwrap().len(), 2);
        let stats = reader.load_stats(claude).unwrap().expect("priced");
        assert_eq!(stats.cost_usd.as_deref(), Some("0.717783"));
        assert_eq!(reader.load_stats(cowork).unwrap(), None, "no token data → None");
        // A 404 usage is advisory → None, not an error.
        let missing = TranscriptHandle {
            session_id: "codex:01a0a70d-eb7f-7b52-9433-c9f5eca9d373".into(),
            path: PathBuf::from("agentsview://x"), modified: Utc::now(),
            reader_name: "agentsview".into(), persona: None, channel: None,
        };
        assert_eq!(reader.load_stats(&missing).unwrap(), None);
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|(_, auth)| auth == "Bearer secret-1"), "{seen:?}");
        assert!(seen.iter().all(|(p, _)| p.starts_with("/api/v1/")), "every request is under /api/v1: {seen:?}");
        assert!(seen.iter().any(|(p, _)| p.starts_with("/api/v1/sessions?limit=500&active_since=2026-09-01T00:00:00Z&include_automated=true&include_one_shot=true&include_children=true")), "{seen:?}");
        assert!(seen.iter().any(|(p, _)| p == "/api/v1/sessions/a37ffc87-2799-4a09-830b-a92fde71d768/messages?from=0&limit=500&direction=asc"), "{seen:?}");
        assert!(seen.iter().any(|(p, _)| p == "/api/v1/sessions/a37ffc87-2799-4a09-830b-a92fde71d768/usage?breakdown=true"), "{seen:?}");
        assert!(seen.iter().any(|(p, _)| p == "/api/v1/machines"), "{seen:?}");
        assert!(seen.iter().any(|(p, _)| p.contains("cursor=CURSOR1")), "second page requested");
        drop(seen);
        // A narrower window: the finished Claude session (ended 2026-09-11)
        // drops out, the still-active cowork session (ended_at null,
        // started 2026-09-15) stays — it is active now.
        let since = DateTime::parse_from_rfc3339("2026-09-16T00:00:00Z").unwrap().with_timezone(&Utc);
        let ids: Vec<String> = reader.discover(since).unwrap().into_iter().map(|h| h.session_id).collect();
        assert_eq!(ids, ["cowork:53c9fd03-e50a-4baa-aadd-cb1c75c60f07"], "{ids:?}");
    }

    #[test]
    fn unreachable_daemon_is_a_reader_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("t");
        std::fs::write(&token_file, "x").unwrap();
        // Port from a listener we immediately drop → connection refused.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let reader = AgentsviewReader::new(&format!("http://127.0.0.1:{port}"), token_file, 7, Duration::from_secs(2)).unwrap();
        let err = reader.discover(Utc::now()).unwrap_err().to_string();
        assert!(err.contains("unreachable"), "{err}");
        assert!(!err.contains("Bearer"), "never echo the header: {err}");
        assert!(reader.probe().is_err());
    }
```

Config tests (`config.rs`):

```rust
    #[test]
    fn agentsview_reader_parses_with_defaults_and_overrides() {
        let cfg = JilogConfig::from_toml_str("[[reader]]\ntype = \"agentsview\"\n").unwrap();
        assert!(matches!(cfg.readers[0], ReaderConfig::Agentsview { url: None, token_file: None, since_days: None, timeout_secs: None, bin: None }));
        assert_eq!(cfg.into_readers()[0].name(), "agentsview");
        let s = cfg.agentsview_settings().expect("agentsview configured");
        assert_eq!(s.url, "http://127.0.0.1:8080");
        assert!(s.token_file.ends_with(".agentsview/config.toml"));
        assert_eq!(s.since_days, 7);
        assert_eq!(s.timeout, std::time::Duration::from_secs(30));
        assert_eq!(s.bin, std::path::PathBuf::from("agentsview"));

        let cfg = JilogConfig::from_toml_str(
            "[[reader]]\ntype = \"agentsview\"\nurl = \"http://100.64.0.9:8080/\"\ntoken_file = \"~/.agentsview/token\"\nsince_days = 3\ntimeout_secs = 5\nbin = \"/opt/homebrew/bin/agentsview\"\n",
        )
        .unwrap();
        assert_eq!(cfg.into_readers().len(), 1);
        let s = cfg.agentsview_settings().unwrap();
        assert_eq!(s.url, "http://100.64.0.9:8080", "trailing slash trimmed");
        assert_eq!(s.bin, std::path::PathBuf::from("/opt/homebrew/bin/agentsview"));
        assert_eq!(s.since_days, 3);
        // No agentsview reader → no settings.
        assert!(JilogConfig::from_toml_str("[[reader]]\ntype = \"pi\"\n").unwrap().agentsview_settings().is_none());
        // https is refused at config load, loudly.
        let err = JilogConfig::from_toml_str("[[reader]]\ntype = \"agentsview\"\nurl = \"https://x\"\n")
            .expect_err("https must be rejected")
            .to_string();
        assert!(err.contains("http://"), "{err}");
    }
```

- [ ] **Step 2: Run** — Expected: compile errors / failures.

- [ ] **Step 3: Implement** — `config.rs`:

```rust
    /// agentsview archive (kenn-io/agentsview): sessions, messages and
    /// per-session usage over the daemon's REST API; also the source of the
    /// digest's archive Spend block (`agentsview usage daily`, run via `bin`).
    /// List it AFTER the raw readers: a session both a raw reader and the
    /// archive know is scanned once, by the raw reader.
    Agentsview {
        /// Daemon base URL, `http://` only (default `http://127.0.0.1:8080`).
        #[serde(default)]
        url: Option<String>,
        /// File holding the bearer token: a TOML with `auth_token` (default
        /// `~/.agentsview/config.toml`) or a bare token. Read per request.
        #[serde(default)]
        token_file: Option<String>,
        /// Archive window cap in days (default 7).
        #[serde(default)]
        since_days: Option<u32>,
        /// Per-request and per-CLI-call timeout (default 30).
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// `agentsview` binary for the daily usage fetch (default: `agentsview` on PATH).
        #[serde(default)]
        bin: Option<String>,
    },
```

Validation in `from_toml_str`, next to the zone check:

```rust
        // agentsview: an http:// IP-literal origin (no TLS, no DNS in the
        // client) and bounded numbers — since_days feeds chrono day
        // arithmetic, timeout_secs feeds Instant + Duration; both must
        // never overflow at run time. Fail at load with the key named.
        for rc in &cfg.readers {
            if let ReaderConfig::Agentsview { url, since_days, timeout_secs, .. } = rc {
                if let Some(url) = url {
                    agentsview::validate_url(url)
                        .map_err(|e| anyhow::anyhow!("reader \"agentsview\": {}", e))?;
                }
                if let Some(d) = since_days {
                    if !(1..=3650).contains(d) {
                        anyhow::bail!("reader \"agentsview\": since_days must be 1..=3650, got {}", d);
                    }
                }
                if let Some(t) = timeout_secs {
                    if !(1..=3600).contains(t) {
                        anyhow::bail!("reader \"agentsview\": timeout_secs must be 1..=3600, got {}", t);
                    }
                }
            }
        }
```

Config tests for the bounds (append to `agentsview_reader_parses_with_defaults_and_overrides`):

```rust
        for bad in [
            "[[reader]]\ntype = \"agentsview\"\nsince_days = 0\n",
            "[[reader]]\ntype = \"agentsview\"\nsince_days = 4000\n",
            "[[reader]]\ntype = \"agentsview\"\ntimeout_secs = 0\n",
            "[[reader]]\ntype = \"agentsview\"\ntimeout_secs = 86400\n",
            "[[reader]]\ntype = \"agentsview\"\nurl = \"http://localhost:8080\"\n",
        ] {
            let err = JilogConfig::from_toml_str(bad).expect_err("out-of-range or hostname must be rejected").to_string();
            assert!(err.contains("agentsview"), "{bad}: {err}");
        }
```

Resolved settings:

```rust
/// The first `agentsview` reader's settings with defaults applied. Used by
/// `into_readers` and by the CLI's archive-spend probe + fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsviewSettings {
    pub url: String,
    pub token_file: PathBuf,
    pub since_days: u32,
    pub timeout: std::time::Duration,
    pub bin: PathBuf,
}

impl JilogConfig {
    pub fn agentsview_settings(&self) -> Option<AgentsviewSettings> {
        self.readers.iter().find_map(|rc| match rc {
            ReaderConfig::Agentsview { url, token_file, since_days, timeout_secs, bin } => Some(AgentsviewSettings {
                url: url.as_deref().unwrap_or(agentsview::DEFAULT_URL).trim_end_matches('/').to_string(),
                token_file: expand_tilde(token_file.as_deref().unwrap_or(agentsview::DEFAULT_TOKEN_FILE)),
                since_days: since_days.unwrap_or(agentsview::DEFAULT_SINCE_DAYS),
                timeout: std::time::Duration::from_secs(timeout_secs.unwrap_or(agentsview::DEFAULT_TIMEOUT_SECS)),
                bin: bin.as_deref().map(expand_tilde).unwrap_or_else(|| PathBuf::from("agentsview")),
            }),
            _ => None,
        })
    }
}
```

`into_readers` stays infallible; the arm (a small helper `agentsview_settings_of(rc)` shares the defaults with the method above, or the arm repeats the five `unwrap_or` lines):

```rust
                    ReaderConfig::Agentsview { url, token_file, since_days, timeout_secs, .. } => {
                        let url = url.as_deref().unwrap_or(agentsview::DEFAULT_URL);
                        let token_file = expand_tilde(token_file.as_deref().unwrap_or(agentsview::DEFAULT_TOKEN_FILE));
                        let since_days = since_days.unwrap_or(agentsview::DEFAULT_SINCE_DAYS);
                        let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(agentsview::DEFAULT_TIMEOUT_SECS));
                        // The url was validated in from_toml_str; a failure here is a programming error.
                        Box::new(
                            AgentsviewReader::new(url, token_file, since_days, timeout)
                                .expect("agentsview url validated at config load"),
                        )
                    }
```

(`use jilog_review::readers::agentsview;` and `AgentsviewReader` in the imports.)

- [ ] **Step 4: Run** `cargo test --workspace && cargo clippy --workspace -- -D warnings` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jilog-review/Cargo.toml crates/jilog-review/src/readers/agentsview.rs crates/jilog-review/src/readers/mod.rs crates/jilog/src/config.rs
git commit -m "Read archived sessions and usage from agentsview over HTTP (jilog#heyg)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

**Acceptance (chunk 3):** parser tests, the stub-server test (exact `/api/v1/...` paths, bearer on every request, both pages, agent + machine tags, probe), the unreachable test, the token-file tests (TOML without `auth_token` → error that does not echo the body) and the config tests (https rejected at load) pass; `cargo tree -p jilog-review` shows `ureq` without `rustls`; no test reaches the network beyond loopback.

---

## Chunk 4 — archive spend: CLI fetch under a timeout, digest block, JSON, CLI wiring

### Task 4.1: `archive_spend` module

**Files:**
- Create: `crates/jilog-review/src/archive_spend.rs`
- Modify: `crates/jilog-review/src/lib.rs` (`pub mod archive_spend; pub use archive_spend::{ArchiveSpend, PeriodSpend, DailyUsage};`)
- Modify: `crates/jilog-review/src/util.rs` (add `run_with_timeout`)

**Interfaces:**
- Produces:
  - `pub struct DailyUsage { pub date: NaiveDate, pub total_usd: Decimal, pub agents: BTreeMap<String, Decimal>, pub models: BTreeMap<String, Decimal> }`
  - `pub fn parse_daily_usage(raw: &str) -> Result<Vec<DailyUsage>, JilogReviewError>`
  - `pub struct PeriodSpend { pub total_usd: Decimal, pub days: usize, pub agents: BTreeMap<String, Decimal>, pub models: BTreeMap<String, Decimal> }`
  - `pub struct ArchiveSpend { pub yesterday: Option<PeriodSpend>, pub week: PeriodSpend, pub week_from: NaiveDate, pub week_to: NaiveDate }` (`Debug, Clone, PartialEq, Eq`)
  - `ArchiveSpend::window(digest_date: NaiveDate) -> (NaiveDate, NaiveDate)` = `(date − 7, date − 1)`
  - `ArchiveSpend::summarize(rows: &[DailyUsage], digest_date: NaiveDate) -> Option<ArchiveSpend>` (`None` when no row falls in the window)
  - `pub fn fetch_daily_usage(bin: &Path, from: NaiveDate, to: NaiveDate, timeout: Duration) -> Result<Vec<DailyUsage>, JilogReviewError>`
  - `util::run_with_timeout(cmd: &mut std::process::Command, timeout: Duration) -> Result<std::process::Output, JilogReviewError>`
  - `PeriodSpend::agents_by_cost(&self) -> Vec<(&String, &Decimal)>` (descending, name tiebreak), `PeriodSpend::top_models(&self, n: usize) -> Vec<(&String, &Decimal)>`

- [ ] **Step 1: Failing tests** (`archive_spend.rs` tests)

```rust
    const DAILY: &str = r#"{"schema_version":6,"pricing":{"source":"fetched"},"projects":{},"daily":[
      {"date":"2026-09-14","inputTokens":1,"outputTokens":2,"cacheCreationTokens":0,"cacheReadTokens":0,"totalCost":{"microdollars":1500000},"modelsUsed":["gpt-5.6-sol"],
       "modelBreakdowns":[{"modelName":"gpt-5.6-sol","cost":{"microdollars":1500000}}],
       "agentBreakdowns":[{"agent":"codex","cost":{"microdollars":1500000}}],"machineBreakdowns":[],"projectBreakdowns":[]},
      {"date":"2026-09-15","inputTokens":12675971,"outputTokens":1834511,"cacheCreationTokens":5395288,"cacheReadTokens":261345608,"totalCost":{"microdollars":332138392},"modelsUsed":["gpt-6-astra","claude-opus-5","gpt-5.6-sol"],
       "modelBreakdowns":[{"modelName":"gpt-6-astra","cost":{"microdollars":125784488}},{"modelName":"claude-opus-5","cost":{"microdollars":107584332}},{"modelName":"gpt-5.6-sol","cost":{"microdollars":98769572}}],
       "agentBreakdowns":[{"agent":"codex","cost":{"microdollars":224554060}},{"agent":"claude","cost":{"microdollars":104943456}},{"agent":"cowork","cost":{"microdollars":2640876}}],
       "machineBreakdowns":[{"machineName":"4a70","cost":{"microdollars":332138392}}],"projectBreakdowns":[]},
      {"date":"2026-09-16","totalCost":{"microdollars":999},"modelBreakdowns":[],"agentBreakdowns":[]}
    ],"totals":{"totalCost":{"microdollars":333638392}},"sessionCounts":{"total":10}}"#;

    #[test]
    fn parses_daily_rows_and_summarizes_yesterday_and_week() {
        let rows = parse_daily_usage(DAILY).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].total_usd.to_string(), "332.138392");
        assert_eq!(rows[1].agents["claude"].to_string(), "104.943456");
        assert_eq!(rows[1].models["gpt-6-astra"].to_string(), "125.784488");

        let digest = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        assert_eq!(ArchiveSpend::window(digest), (NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()));
        let spend = ArchiveSpend::summarize(&rows, digest).expect("rows in window");
        let y = spend.yesterday.as_ref().expect("2026-09-15 present");
        assert_eq!(y.total_usd.to_string(), "332.138392");
        assert_eq!(spend.week.days, 2, "the 16th is today and excluded");
        assert_eq!(spend.week.total_usd.to_string(), "333.638392");
        assert_eq!(spend.week.agents["codex"].to_string(), "226.054060");
        let agents: Vec<&str> = spend.week.agents_by_cost().iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(agents, ["codex", "claude", "cowork"]);
        let models: Vec<&str> = spend.week.top_models(2).iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(models, ["gpt-6-astra", "claude-opus-5"]);

        // Yesterday absent → None for yesterday, week still summarized.
        let later = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let spend = ArchiveSpend::summarize(&rows, later).unwrap();
        assert!(spend.yesterday.is_none());
        assert_eq!(spend.week.days, 3);
        // Nothing in window → None; an empty daily array → no rows → None.
        assert!(ArchiveSpend::summarize(&rows, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()).is_none());
        assert!(ArchiveSpend::summarize(&[], digest).is_none());
        assert_eq!(parse_daily_usage("{\"daily\": []}").unwrap().len(), 0);
        // Window edges: a row dated exactly digest − 7 is in, digest − 8 is out.
        let edge = |d: u32| DailyUsage { date: NaiveDate::from_ymd_opt(2026, 9, d).unwrap(), total_usd: Decimal::ONE, agents: Default::default(), models: Default::default() };
        let s = ArchiveSpend::summarize(&[edge(8), edge(9)], digest).unwrap();
        assert_eq!(s.week.days, 1, "2026-09-08 is outside the 7-day window ending 2026-09-15");
        // Top-5 truncation.
        let mut many = DailyUsage { date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(), total_usd: Decimal::ONE, agents: Default::default(), models: Default::default() };
        for i in 0..8 { many.models.insert(format!("m{i}"), Decimal::from(i)); }
        let s = ArchiveSpend::summarize(&[many], digest).unwrap();
        let top: Vec<&str> = s.week.top_models(5).iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(top, ["m7", "m6", "m5", "m4", "m3"]);
        // Garbage → Err, not a panic.
        assert!(parse_daily_usage("not json").is_err());
        assert!(parse_daily_usage("{\"daily\": 5}").is_err());
    }

    #[test]
    fn fetch_uses_the_binary_with_window_flags_and_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let stub = dir.path().join("agentsview");
        std::fs::write(&stub, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncat <<'EOF'\n{}\nEOF\n", calls.display(), DAILY)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let from = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let rows = fetch_daily_usage(&stub, from, to, Duration::from_secs(10)).unwrap();
        assert_eq!(rows.len(), 3);
        let argv = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(argv.trim(), "usage daily --json --breakdown --since 2026-09-09 --until 2026-09-15 --no-sync");

        // A hung binary is killed at the deadline.
        let slow = dir.path().join("slow");
        std::fs::write(&slow, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let started = std::time::Instant::now();
        let err = fetch_daily_usage(&slow, from, to, Duration::from_millis(300)).unwrap_err().to_string();
        assert!(started.elapsed() < Duration::from_secs(5), "must not wait for the child");
        assert!(err.contains("timed out"), "{err}");
        // Missing binary → Err.
        assert!(fetch_daily_usage(&dir.path().join("missing"), from, to, Duration::from_secs(1)).is_err());
        // Non-zero exit → Err naming the status.
        let bad = dir.path().join("bad");
        std::fs::write(&bad, "#!/bin/sh\necho 'fatal: no archive' >&2\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = fetch_daily_usage(&bad, from, to, Duration::from_secs(5)).unwrap_err().to_string();
        assert!(err.contains("exit") && err.contains("no archive"), "{err}");
    }
```

- [ ] **Step 2: Run** — Expected: compile errors.

- [ ] **Step 3: Implement**

`util.rs` (add `libc = { workspace = true }` to `crates/jilog-review/Cargo.toml` `[dependencies]` — the workspace already pins it):

```rust
/// Run `cmd` with stdin closed and both pipes captured, bounded by
/// `timeout` end to end: the child runs in its own process group (unix) so
/// a descendant that inherited a pipe dies with it, the exit wait and the
/// pipe drain share one deadline, and on expiry the whole group is killed.
/// A daemon mid-sync makes `agentsview usage daily` block for minutes
/// (observed 2026-09-16); the nightly must never wait on it.
pub fn run_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, crate::error::JilogReviewError> {
    use crate::error::JilogReviewError;
    use std::io::Read as _;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Instant;

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| JilogReviewError::Reader("timeout too large".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pgid = child.id();
    let kill_group = |child: &mut std::process::Child| {
        #[cfg(unix)]
        // SAFETY: plain libc call; a negative pid addresses the process
        // group we created with process_group(0), whose id is the child's pid.
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let tx_out = tx.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx_out.send((0, buf));
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = tx.send((1, buf));
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            return Err(JilogReviewError::Reader(format!(
                "command timed out after {}s",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    // Drain both pipes under the same deadline: a descendant that inherited
    // a pipe keeps it open after the direct child exits.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok((0, buf)) => out = buf,
            Ok((_, buf)) => err = buf,
            Err(_) => {
                kill_group(&mut child);
                return Err(JilogReviewError::Reader(format!(
                    "command timed out after {}s waiting for output (a descendant kept the pipe open)",
                    timeout.as_secs()
                )));
            }
        }
    }
    Ok(std::process::Output { status, stdout: out, stderr: err })
}
```

(`JilogReviewError` has `From<std::io::Error>` — check `error.rs`; it does, `#[from] std::io::Error`. `CommandExt::process_group` is stable since Rust 1.64; rust-version is 1.88 after this change.) Test in `util.rs` (unix only):

```rust
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_returns_inside_the_deadline_when_a_descendant_holds_the_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("orphan.sh");
        // The direct child exits at once; its background grandchild keeps
        // stdout open for 30s. Without a group kill + bounded drain this
        // would block on the pipe until the grandchild exits.
        std::fs::write(&script, "#!/bin/sh\n( sleep 30 ) &\necho started\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let err = run_with_timeout(&mut std::process::Command::new(&script), std::time::Duration::from_millis(500))
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "returned in {:?}", started.elapsed());
        assert!(err.contains("timed out"), "{err}");
        // A well-behaved child returns its output and status.
        let ok = dir.path().join("ok.sh");
        std::fs::write(&ok, "#!/bin/sh\necho out\necho err >&2\nexit 0\n").unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = run_with_timeout(&mut std::process::Command::new(&ok), std::time::Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
    }
```

`archive_spend.rs`:

```rust
//! Archive spend — the digest's "Archive spend (agentsview)" block, built
//! from `agentsview usage daily --json` (yesterday + trailing 7 days, per
//! agent, top models). Money is `rust_decimal` from `microdollars`.
//!
//! Nothing here may stall or fail the nightly: the CLI runs under a hard
//! timeout and every failure is returned as an error the caller turns into
//! a warning (the block is simply absent).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::error::JilogReviewError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub total_usd: Decimal,
    pub agents: BTreeMap<String, Decimal>,
    pub models: BTreeMap<String, Decimal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodSpend {
    pub total_usd: Decimal,
    /// Days in the period that had an archive row.
    pub days: usize,
    pub agents: BTreeMap<String, Decimal>,
    pub models: BTreeMap<String, Decimal>,
}

impl PeriodSpend {
    fn add(&mut self, row: &DailyUsage) {
        self.days += 1;
        self.total_usd += row.total_usd;
        for (k, v) in &row.agents {
            *self.agents.entry(k.clone()).or_insert(Decimal::ZERO) += *v;
        }
        for (k, v) in &row.models {
            *self.models.entry(k.clone()).or_insert(Decimal::ZERO) += *v;
        }
    }

    /// Agents by cost, descending; ties by name.
    pub fn agents_by_cost(&self) -> Vec<(&String, &Decimal)> {
        let mut v: Vec<(&String, &Decimal)> = self.agents.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    /// The `n` most expensive models, descending; ties by name.
    pub fn top_models(&self, n: usize) -> Vec<(&String, &Decimal)> {
        let mut v: Vec<(&String, &Decimal)> = self.models.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        v.truncate(n);
        v
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSpend {
    /// The day before the digest date, when the archive had a row for it.
    pub yesterday: Option<PeriodSpend>,
    /// The seven days ending yesterday (rows present only).
    pub week: PeriodSpend,
    pub week_from: NaiveDate,
    pub week_to: NaiveDate,
}

impl ArchiveSpend {
    /// `(digest_date − 7, digest_date − 1)`.
    pub fn window(digest_date: NaiveDate) -> (NaiveDate, NaiveDate) {
        (digest_date - chrono::Duration::days(7), digest_date - chrono::Duration::days(1))
    }

    pub fn summarize(rows: &[DailyUsage], digest_date: NaiveDate) -> Option<Self> {
        let (from, to) = Self::window(digest_date);
        let mut week = PeriodSpend::default();
        let mut yesterday: Option<PeriodSpend> = None;
        for row in rows.iter().filter(|r| r.date >= from && r.date <= to) {
            week.add(row);
            if row.date == to {
                let mut y = PeriodSpend::default();
                y.add(row);
                yesterday = Some(y);
            }
        }
        if week.days == 0 {
            return None;
        }
        Some(Self { yesterday, week, week_from: from, week_to: to })
    }
}

fn micro(v: &serde_json::Value, key: &str) -> Decimal {
    Decimal::new(v.get(key).and_then(|c| c.get("microdollars")).and_then(|m| m.as_i64()).unwrap_or(0), 6)
}

fn breakdown(v: &serde_json::Value, list: &str, name: &str) -> BTreeMap<String, Decimal> {
    let mut out = BTreeMap::new();
    for row in v.get(list).and_then(|b| b.as_array()).into_iter().flatten() {
        if let Some(k) = row.get(name).and_then(|n| n.as_str()) {
            *out.entry(k.to_string()).or_insert(Decimal::ZERO) += micro(row, "cost");
        }
    }
    out
}

/// Parse `agentsview usage daily --json` output (schema_version 6).
pub fn parse_daily_usage(raw: &str) -> Result<Vec<DailyUsage>, JilogReviewError> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| JilogReviewError::Reader(format!("agentsview usage daily: bad JSON: {}", e)))?;
    let daily = v
        .get("daily")
        .and_then(|d| d.as_array())
        .ok_or_else(|| JilogReviewError::Reader("agentsview usage daily: no `daily` array".into()))?;
    let mut rows = Vec::with_capacity(daily.len());
    for row in daily {
        let date = match row.get("date").and_then(|d| d.as_str()).and_then(|s| s.parse::<NaiveDate>().ok()) {
            Some(d) => d,
            None => {
                tracing::warn!("agentsview usage daily: row without a date skipped");
                continue;
            }
        };
        rows.push(DailyUsage {
            date,
            total_usd: micro(row, "totalCost"),
            agents: breakdown(row, "agentBreakdowns", "agent"),
            models: breakdown(row, "modelBreakdowns", "modelName"),
        });
    }
    Ok(rows)
}

/// Run `<bin> usage daily --json --breakdown --since <from> --until <to>
/// --no-sync` under `timeout` and parse it. `--breakdown` is what fills
/// `agentBreakdowns` (model rows are always present); `--no-sync` keeps the
/// CLI from spawning or waiting on a daemon sync of its own.
pub fn fetch_daily_usage(bin: &Path, from: NaiveDate, to: NaiveDate, timeout: Duration) -> Result<Vec<DailyUsage>, JilogReviewError> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["usage", "daily", "--json", "--breakdown", "--since", &from.to_string(), "--until", &to.to_string(), "--no-sync"]);
    let out = crate::util::run_with_timeout(&mut cmd, timeout)
        .map_err(|e| JilogReviewError::Reader(format!("agentsview usage daily ({}): {}", bin.display(), e)))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(JilogReviewError::Reader(format!(
            "agentsview usage daily exit {}: {}",
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            stderr.lines().next().unwrap_or("").trim()
        )));
    }
    parse_daily_usage(&String::from_utf8_lossy(&out.stdout))
}
```

(`Decimal` supports `+=` and `Ord`, so `cmp` on `&Decimal` works; `NaiveDate: FromStr` parses `YYYY-MM-DD`.)

- [ ] **Step 4: Run** `cargo test -p jilog-review archive_spend && cargo test -p jilog-review run_with_timeout` — Expected: PASS.

### Task 4.2: Thread `ArchiveSpend` through `ReviewArgs`, `DigestReport`, the renderer and the CLI

**Files:**
- Modify: `crates/jilog-review/src/digest.rs` (`ReviewArgs` `:28-42`, `DigestReport` `:44-70`, `run_review` tail `:618-633`, `render_digest` `:652-663` + `:853-897`, `write_digest` `:901-923`, every `ReviewArgs {` / `write_digest(` / `render_digest(` in tests)
- Modify: `crates/jilog/src/commands/review.rs` (`run_nightly` `:74-120`, `digest_report_json` `:190+`, tests `:279-295` `nightly_args`/`digest_report`; the key-set assertion at `:383` stays as written)
- Modify: `crates/jilog-review/src/lib.rs` re-exports

**Interfaces:**
- Consumes: `ArchiveSpend`, `fetch_daily_usage`, `AgentsviewReader::probe`, `JilogConfig::agentsview_settings`.
- Produces: `ReviewArgs.archive_spend: Option<ArchiveSpend>`; `DigestReport.archive_spend: Option<ArchiveSpend>`; `render_digest(date, corrections, errors, workarounds, deferrals, patterns, p0_alerts, spend, archive_spend: Option<&ArchiveSpend>, recurrence_costs, issue_index, personas)`; `write_digest(..., spend, archive_spend, recurrence_costs, digest_dir, issue_index, personas)`; review JSON key `archive_spend` present ONLY when the report carries one; `pub fn load_archive_spend(settings: &AgentsviewSettings, digest_date: NaiveDate) -> Option<ArchiveSpend>` in `commands/review.rs` (probe → fetch → summarize; every failure logs one warning and returns `None`).

- [ ] **Step 1: Failing tests**

`digest.rs`:

```rust
    fn sample_archive_spend() -> crate::archive_spend::ArchiveSpend {
        use crate::archive_spend::{ArchiveSpend, PeriodSpend};
        let d = |s: &str| Decimal::from_str(s).unwrap();
        let mut week = PeriodSpend { total_usd: d("2101.5"), days: 7, ..Default::default() };
        week.agents.insert("codex".into(), d("1300.25"));
        week.agents.insert("claude".into(), d("800.25"));
        week.agents.insert("cowork".into(), d("1"));
        for (m, c) in [("gpt-6-astra", "900"), ("claude-opus-5", "700.5"), ("gpt-5.6-sol", "400"), ("claude-haiku-4-5-20251001", "60"), ("m5", "30"), ("m6", "11")] {
            week.models.insert(m.into(), d(c));
        }
        let mut yesterday = PeriodSpend { total_usd: d("332.138392"), days: 1, ..Default::default() };
        yesterday.agents.insert("codex".into(), d("224.55406"));
        yesterday.agents.insert("claude".into(), d("104.943456"));
        yesterday.agents.insert("cowork".into(), d("2.640876"));
        ArchiveSpend {
            yesterday: Some(yesterday),
            week,
            week_from: NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(),
            week_to: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
        }
    }

    #[test]
    fn digest_archive_spend_block_renders_after_observed_spend() {
        let archive = sample_archive_spend();
        let body = render_digest(
            "2026-09-16", &[], &[], &[], &[], &[], &HashMap::new(),
            None, Some(&archive), &HashMap::new(), &no_issues(), &BTreeMap::new(),
        );
        let expected = "## Spend\n\n### Archive spend (agentsview)\n\n\
- **Yesterday (2026-09-15)**: $332.138392 — codex $224.55406, claude $104.943456, cowork $2.640876\n\
- **Trailing 7d (2026-09-09 – 2026-09-15)**: $2101.50 across 7 day(s) — codex $1300.25, claude $800.25, cowork $1.00\n\
- **Top models (7d)**: `gpt-6-astra` $900.00, `claude-opus-5` $700.50, `gpt-5.6-sol` $400.00, `claude-haiku-4-5-20251001` $60.00, `m5` $30.00\n\n";
        assert!(body.ends_with(expected), "archive block:\n{body}");
        // With observed stats too: the observed block is unchanged and comes first.
        let spend = SpendSummary { sessions_with_stats: 2, input_tokens: 10, output_tokens: 5, ..Default::default() };
        let body = render_digest(
            "2026-09-16", &[], &[], &[], &[], &[], &HashMap::new(),
            Some(&spend), Some(&archive), &HashMap::new(), &no_issues(), &BTreeMap::new(),
        );
        assert!(body.contains("## Spend\n\n- **Total**: no cost data (2 session(s) with usage; unpriced models)\n- **Tokens**: 10 in / 5 out\n\n### Archive spend (agentsview)\n"), "{body}");
        // Yesterday absent.
        let mut no_yesterday = archive.clone();
        no_yesterday.yesterday = None;
        let body = render_digest(
            "2026-09-16", &[], &[], &[], &[], &[], &HashMap::new(),
            None, Some(&no_yesterday), &HashMap::new(), &no_issues(), &BTreeMap::new(),
        );
        assert!(body.contains("- **Yesterday (2026-09-15)**: no archive rows\n"), "{body}");
    }

    #[test]
    fn digest_without_archive_spend_is_unchanged() {
        let body = render_digest(
            "2026-09-16", &[], &[], &[], &[], &[], &HashMap::new(),
            None, None, &HashMap::new(), &no_issues(), &BTreeMap::new(),
        );
        assert!(!body.contains("## Spend"), "{body}");
        assert!(body.ends_with("## Patterns\n\n_No patterns detected._\n\n"), "{body}");
    }
```

`review.rs` test (append):

```rust
    /// One-shot loopback daemon stand-in: answers every request 200 with
    /// `body`, then stops. Enough for the probe.
    fn one_shot_daemon(body: &'static str) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let mut stream = match stream { Ok(s) => s, Err(_) => break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                }
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        base
    }

    #[test]
    fn nightly_survives_an_unreachable_agentsview() {
        // The agentsview reader points at a closed port: the probe fails,
        // the block is hidden, and a full (non-dry) run still writes a
        // digest and exits Ok.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, "t").unwrap();
        let cfg = JilogConfig::from_toml_str(&format!(
            "[[reader]]\ntype = \"agentsview\"\nurl = \"http://127.0.0.1:{port}\"\ntoken_file = \"{}\"\ntimeout_secs = 2\nbin = \"{}\"\n",
            token.display(),
            dir.path().join("no-such-agentsview").display()
        ))
        .unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let settings = cfg.agentsview_settings().unwrap();
        assert!(load_archive_spend(&settings, date).is_none(), "probe fails → hidden");
        let mut args = nightly_args();
        args.dry_run = false;
        args.date = Some(date);
        args.digest_dir = Some(dir.path().join("digests"));
        args.processed_file = Some(dir.path().join("processed.txt"));
        run_nightly(&cfg, &args).expect("agentsview outage never fails the nightly");
        let digest = std::fs::read_to_string(dir.path().join("digests/learning-digest-2026-09-16.md")).unwrap();
        assert!(!digest.contains("Archive spend"), "{digest}");
        assert!(!digest.contains("## Spend"), "{digest}");
    }

    #[test]
    fn archive_spend_hidden_when_the_binary_is_missing_even_if_the_daemon_answers() {
        // Probe succeeds (stub answers /api/v1/machines) but `bin` does not
        // exist → fetch fails → None, no panic.
        let base = one_shot_daemon(r#"{"machines":[],"machine_labels":{},"machine_aliases":{}}"#);
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, "t").unwrap();
        let cfg = JilogConfig::from_toml_str(&format!(
            "[[reader]]\ntype = \"agentsview\"\nurl = \"{base}\"\ntoken_file = \"{}\"\ntimeout_secs = 2\nbin = \"{}\"\n",
            token.display(),
            dir.path().join("no-such-agentsview").display()
        ))
        .unwrap();
        let settings = cfg.agentsview_settings().unwrap();
        assert!(load_archive_spend(&settings, chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap()).is_none());
        // And the JSON document for a report without the block has no key.
        let value = digest_report_json(&digest_report(), false);
        assert!(value.get("archive_spend").is_none());
    }

    #[test]
    fn review_json_carries_archive_spend_only_when_present() {
        let mut report = digest_report();
        let value = digest_report_json(&report, false);
        assert!(value.get("archive_spend").is_none(), "no key when absent — unconfigured hosts keep today's document");
        let mut week = jilog_review::PeriodSpend::default();
        week.total_usd = rust_decimal::Decimal::new(2101500000, 6);
        week.days = 7;
        week.agents.insert("codex".into(), rust_decimal::Decimal::new(1300250000, 6));
        report.archive_spend = Some(jilog_review::ArchiveSpend {
            yesterday: None,
            week,
            week_from: chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(),
            week_to: chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
        });
        let value = digest_report_json(&report, false);
        assert_eq!(value["archive_spend"]["week"]["total_usd"], "2101.500000");
        assert_eq!(value["archive_spend"]["week"]["days"], 7);
        assert_eq!(value["archive_spend"]["week"]["agents_usd"]["codex"], "1300.250000");
        assert_eq!(value["archive_spend"]["week_from"], "2026-09-09");
        assert!(value["archive_spend"]["yesterday"].is_null());
        assert_eq!(value["schema_version"], 2);
    }
```

(`rust_decimal` must be added to `crates/jilog/Cargo.toml` `[dependencies]` as `rust_decimal = { workspace = true }`.)

- [ ] **Step 2: Run** — Expected: compile errors (`archive_spend` field/param missing).

- [ ] **Step 3: Implement**

`ReviewArgs` gains, after `create_issues`:

```rust
    /// Archive spend from `agentsview usage daily`, fetched by the caller
    /// (the CLI) before the run; None when agentsview is not configured or
    /// was unavailable. Rendered as the digest's "Archive spend" block.
    pub archive_spend: Option<crate::archive_spend::ArchiveSpend>,
```

`DigestReport` gains `pub archive_spend: Option<crate::archive_spend::ArchiveSpend>` after `spend`; `run_review` passes `args.archive_spend.as_ref()` to `write_digest` and `args.archive_spend.clone()` into the report. `render_digest`'s Spend section becomes:

```rust
    // Spend — rendered when at least one session reported stats OR the
    // archive block is available. No empty section otherwise: message-only
    // readers without agentsview stay silent here, byte-identical to before.
    if spend.is_some() || archive_spend.is_some() {
        buf.push_str("## Spend\n\n");
        if let Some(sp) = spend {
            /* the existing body of the `if let Some(sp) = spend` block, unchanged */
        }
        if let Some(a) = archive_spend {
            render_archive_spend(&mut buf, a);
        }
    }
```

and the helper:

```rust
/// The "Archive spend (agentsview)" block: yesterday, trailing 7d, top models.
fn render_archive_spend(buf: &mut String, a: &crate::archive_spend::ArchiveSpend) {
    fn agents(p: &crate::archive_spend::PeriodSpend) -> String {
        p.agents_by_cost()
            .iter()
            .map(|(agent, cost)| format!("{} {}", sanitize_display(agent), format_usd(cost)))
            .collect::<Vec<_>>()
            .join(", ")
    }
    buf.push_str("### Archive spend (agentsview)\n\n");
    match &a.yesterday {
        Some(y) => buf.push_str(&format!(
            "- **Yesterday ({})**: {} — {}\n",
            a.week_to, format_usd(&y.total_usd), agents(y)
        )),
        None => buf.push_str(&format!("- **Yesterday ({})**: no archive rows\n", a.week_to)),
    }
    buf.push_str(&format!(
        "- **Trailing 7d ({} – {})**: {} across {} day(s) — {}\n",
        a.week_from, a.week_to, format_usd(&a.week.total_usd), a.week.days, agents(&a.week)
    ));
    let models = a
        .week
        .top_models(5)
        .iter()
        .map(|(m, c)| format!("`{}` {}", sanitize_display(m), format_usd(c)))
        .collect::<Vec<_>>()
        .join(", ");
    if !models.is_empty() {
        buf.push_str(&format!("- **Top models (7d)**: {}\n", models));
    }
    buf.push('\n');
}
```

(An agent line with no agents renders `— ` followed by nothing; the archive always has at least one agent row when a day has cost, so leave it.) `NaiveDate`'s `Display` is `YYYY-MM-DD`.

Every `ReviewArgs { … }` literal in tests gains `archive_spend: None,`; every `render_digest(`/`write_digest(` call in tests gains `None,` after the `spend` argument (test `digest_spend_section_renders_totals_roles_models` and friends at `:1323-1380`, the `run_review` tests, `review.rs` `digest_report()` gains `archive_spend: None`).

`review.rs`:

```rust
/// The digest's archive Spend block, or None. Advisory by contract: the
/// daemon is probed first (a `usage daily` run answers from the local
/// archive even when the daemon is down, which would render stale numbers
/// the brief says to hide), then the CLI runs under the timeout. Every
/// failure — unreachable, no binary, mid-sync block, timeout, bad JSON,
/// empty window — logs one warning and hides the block; the run goes on.
pub fn load_archive_spend(settings: &crate::config::AgentsviewSettings, date: NaiveDate) -> Option<jilog_review::ArchiveSpend> {
    let reader = match jilog_review::readers::AgentsviewReader::new(
        &settings.url, settings.token_file.clone(), settings.since_days, settings.timeout,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("archive spend hidden: {}", e);
            return None;
        }
    };
    if let Err(e) = reader.probe() {
        tracing::warn!("archive spend hidden: agentsview daemon not reachable: {}", e);
        return None;
    }
    let (from, to) = jilog_review::ArchiveSpend::window(date);
    match jilog_review::archive_spend::fetch_daily_usage(&settings.bin, from, to, settings.timeout) {
        Ok(rows) => {
            let spend = jilog_review::ArchiveSpend::summarize(&rows, date);
            if spend.is_none() {
                tracing::warn!("archive spend hidden: no usage rows between {} and {}", from, to);
            }
            spend
        }
        Err(e) => {
            tracing::warn!("archive spend hidden: {}", e);
            None
        }
    }
}
```

In `run_nightly`, before building `review_args`: `let archive_spend = cfg.agentsview_settings().and_then(|s| load_archive_spend(&s, date));` and `archive_spend` in `LibReviewArgs`. In the human summary, after the existing `Spend:` line:

```rust
        if let Some(a) = &report.archive_spend {
            println!(
                "Archive spend: {} yesterday, {} trailing 7d ({} day(s))",
                a.yesterday.as_ref().map(|y| format!("${}", y.total_usd)).unwrap_or_else(|| "n/a".into()),
                a.week.total_usd, a.week.days
            );
        }
```

`digest_report_json` adds:

```rust
    let period = |p: &jilog_review::PeriodSpend| serde_json::json!({
        "total_usd": p.total_usd.to_string(),
        "days": p.days,
        "agents_usd": p.agents.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string()))).collect::<serde_json::Map<_, _>>(),
        "models_usd": p.models.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string()))).collect::<serde_json::Map<_, _>>(),
    });
    let mut value = serde_json::json!({ /* the existing object, unchanged */ });
    // Present only when the block rendered: an unconfigured host's document
    // keeps today's exact key set (the key-set test below guards it).
    if let Some(a) = &report.archive_spend {
        value["archive_spend"] = serde_json::json!({
            "yesterday": a.yesterday.as_ref().map(period),
            "week": period(&a.week),
            "week_from": a.week_from.to_string(),
            "week_to": a.week_to.to_string(),
        });
    }
    value
```

`lib.rs`: `pub mod archive_spend; pub use archive_spend::{ArchiveSpend, DailyUsage, PeriodSpend};`.

- [ ] **Step 4: Run** `cargo test --workspace && cargo clippy --workspace -- -D warnings` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jilog-review/src/archive_spend.rs crates/jilog-review/src/util.rs crates/jilog-review/src/lib.rs crates/jilog-review/src/digest.rs crates/jilog/src/commands/review.rs crates/jilog/Cargo.toml Cargo.lock
git commit -m "Add the archive spend block from agentsview usage daily (jilog#heyg)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

**Acceptance (chunk 4):** the renderer tests pass with the exact expected bytes; `digest_without_archive_spend_is_unchanged`, the existing key-set test in `review.rs` and every pre-existing digest test pass unchanged; the timeout test finishes in under five seconds; `nightly_survives_an_unreachable_agentsview_and_a_missing_binary` passes; `review nightly --json` has no `archive_spend` key without agentsview.

---

## Chunk 5 — docs, version, live config, install, live verification

### Task 5.1: README, version bump

**Files:**
- Modify: `README.md` (readers table `claude-code` row; new `agentsview` row; the codex/worker-signals config example; "Cost-weighted digests" section; a new "agentsview archive" subsection)
- Modify: `Cargo.toml` (`version = "0.8.0"`), `Cargo.lock` (via `cargo build`)

- [ ] **Step 1:** README edits.

Readers table rows:

```markdown
| `claude-code` | `~/.claude/projects/**/*.jsonl` (`{type, message: {role, content}}` wrapper format). `paths = [...]` scans several roots; `discover_profiles = true` adds every `~/.claude-pool/profiles/*/projects` and `~/.claude-profiles/*/projects` found at scan time, tagged `seat = <profile dir>` | — | ✅ built-in |
| `agentsview` | The [agentsview](https://github.com/kenn-io/agentsview) archive over its REST API (`http://127.0.0.1:8080` by default, bearer token from `~/.agentsview/config.toml` read per request): every agent it syncs (Claude Code seats and profiles, Codex pool, cowork, cursor, copilot, hermes, pi, …) and every machine it collects from, tagged `machine = <label>`; per-session usage → spend; list it after the raw readers (session-id dedupe) | — | ✅ built-in |
```

Config example (after the codex/worker-signals block):

```toml
# Claude Code across the pool seats and context profiles on this Mac
[[reader]]
type = "claude-code"
path = "~/.claude/projects"
discover_profiles = true      # + ~/.claude-pool/profiles/*/projects, ~/.claude-profiles/*/projects
# or: paths = ["~/.claude/projects", "/archive/seat-01/projects"]

# The agentsview archive: other machines and agents without a raw reader,
# plus the digest's "Archive spend" block. Keep it LAST — a session both a
# raw reader and the archive know is scanned once, by the raw reader.
[[reader]]
type = "agentsview"
# url = "http://127.0.0.1:8080"            # http:// only (no TLS)
# token_file = "~/.agentsview/config.toml"  # auth_token = "…", or a bare token file
# since_days = 7                            # archive window cap
# timeout_secs = 30                         # per request and for `agentsview usage daily`
# bin = "agentsview"                        # for the daily spend fetch
```

New subsection under "Cost-weighted digests":

```markdown
### Archive spend (agentsview)

With an `agentsview` reader configured, the nightly also runs `agentsview usage daily --json --breakdown --since <date−7> --until <date−1> --no-sync` and adds an **Archive spend** block to the Spend section: yesterday's total per agent, the trailing seven days per agent, and the five most expensive models. Costs come from agentsview's own pricing (`microdollars`, summed with `rust_decimal`); jilog still keeps no price tables. The block — and the `archive_spend` key in `--json` — is absent when agentsview is not configured, the binary is missing, the daemon is unreachable or mid-sync, the call exceeds `timeout_secs`, the output fails structural validation (schema_version 6, integer microdollars, both breakdown arrays), or the window has no rows. None of those fail the run. Archive signals carry `` `agent:<name>` `machine:<label>` `` spans in their digest line and `agent`/`machine` fields in JSON; local readers leave both absent.
```

Update the "Readers" dedupe paragraph: "Explicit `path` or `paths` replaces automatic root discovery; an empty `paths` list scans nothing." now applies to both codex and claude-code — say so.

- [ ] **Step 2:** `Cargo.toml` version `0.8.0`; `cargo build --release` refreshes `Cargo.lock`.

- [ ] **Step 3: Run** `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo build --release` — Expected: PASS; `./target/release/jilog --version` → `jilog 0.8.0`.

- [ ] **Step 4: Commit**

```bash
git add README.md Cargo.toml Cargo.lock
git commit -m "Document the agentsview reader and multi-root claude-code scanning; 0.8.0 (jilog#heyg)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

### Task 5.2: Live config, install, verification (not a code task; run by the pipeline owner after the code chunks pass review)

- [ ] **Step 1:** Edit `~/.jilog.toml` (back it up first to `~/.jilog.toml.bak-heyg`): add `discover_profiles = true` under the `claude-code` reader and append after the `worker-signals` block:

```toml
# agentsview archive (jilog#heyg, jibot-code#mdxg): pool seats and profiles
# on other Macs, agents with no raw reader, and the digest's Archive spend
# block. LAST on purpose — sessions a raw reader above already scanned are
# skipped by session id. Token read per request from ~/.agentsview/config.toml.
[[reader]]
type = "agentsview"
```

- [ ] **Step 2:** `./target/release/jilog review nightly --digest-dir /tmp/jilog-heyg-digest --processed-file /tmp/jilog-heyg-digest/verify-processed.txt` (no `--create-issues`; the scratch `--processed-file` is mandatory — the default is the live dedup ledger, and marking it would hide those sessions from the real nightly). Confirm: pool-seat sessions appear (`seat:seat-NN` spans or a scanned count above the pre-change run), the Spend section carries the archive block, and the run exits 0. Paste the Spend section into the kata comment.

- [ ] **Step 3:** Negative check — an agentsview reader on a port nothing listens on (port 1, tcpmux, is closed on macOS by default) and a binary that does not exist:

```bash
mkdir -p /tmp/jilog-heyg-digest
cat > /tmp/jilog-heyg-digest/bad.toml <<'EOF'
[[reader]]
type = "agentsview"
url = "http://127.0.0.1:1"
token_file = "/tmp/jilog-heyg-digest/token"
timeout_secs = 5
bin = "/nonexistent/agentsview"

[tracker]
type = "none"
EOF
printf 'not-a-real-token\n' > /tmp/jilog-heyg-digest/token
./target/release/jilog --config /tmp/jilog-heyg-digest/bad.toml review nightly --dry-run \
  --processed-file /tmp/jilog-heyg-digest/bad-processed.txt; echo "exit=$?"
```

Expected: three `WARN` lines (`archive spend hidden: agentsview daemon not reachable`, `agentsview: machine labels unavailable` from inside discover, and `reader 'agentsview' discover failed`), the summary line `0 corrections, 0 errors, … 0 session(s) scanned`, and `exit=0`. Paste the four lines into the kata close-out comment.

- [ ] **Step 4:** Install the way it is installed today: `cargo install --path crates/jilog` from the primary checkout after `main` is fast-forwarded (`cargo install --list` shows `jilog v0.7.2 (/Users/joi/repos/jilog/crates/jilog)`), then `~/.local/bin/jilog --version` → `jilog 0.8.0`.

- [ ] **Step 5:** File the follow-up for the authoritative repo copy (amplifier-bundle-joi `config/jilog/macazbd/jilog.toml`, repoman-managed) with the exact diff, `--idempotency-key heyg-jilog-toml-repo-copy-2026-09-16`.

**Acceptance (chunk 5):** README documents every new key; `jilog 0.8.0` installed; the live digest run shows seat-tagged pool sessions and the archive spend block; a closed-port config warns and exits 0.

---

## Rollback

- Code: `git revert` the chunk commits in reverse order (5 → 1); each is self-contained.
- Config: remove `discover_profiles = true` and the `[[reader]] type = "agentsview"` block from `~/.jilog.toml` (or restore `~/.jilog.toml.bak-heyg`); a pre-0.8 binary rejects both keys at parse time, so the config must be rolled back with the binary.
- Install: `cargo install --path crates/jilog` from the reverted `main`.
- Processed file: extra dedupe-key lines are inert.

## Self-review

- Spec coverage: §1 → Task 1.1 (incl. explicit-root precedence); §2 → Tasks 2.1–2.2 (agent + machine, alias unmark, retry test); §3 → Tasks 3.1–3.2 (token rule, role filter, probe, `/api/v1` paths, config-load validation); §4 → Tasks 4.1–4.2 (probe-gated fetch, key omitted when absent, outage test); §5 → Tasks 5.1–5.2; lens → `digest_without_archive_spend_is_unchanged`, the untouched key-set test in `review.rs`, and every existing test unchanged; "never fail" → `run_review`'s existing discover warning, `load_archive_spend` returning `None` on every failure, `run_with_timeout`, and the CLI-level outage test.
- Type consistency: `ArchiveSpend { yesterday, week, week_from, week_to }`, `PeriodSpend { total_usd, days, agents, models }`, `agents_by_cost()`, `top_models(n)`, `ArchiveSpend::window`, `summarize`, `fetch_daily_usage(bin, from, to, timeout)`, `run_with_timeout(cmd, timeout)`, `validate_url`, `AgentsviewReader::new(url, token_file, since_days, timeout)`, `probe()`, `read_token`, `strip_agent_prefix`, `microdollars_to_usd`, `parse_*`, `Reader::agent`, `Reader::machine`, `Reader::dedupe_key`, `trailing_uuid`, `DEFAULT_PROFILE_PARENTS`, `AgentsviewSettings`, `agentsview_settings()`, `load_archive_spend(settings, date)` are used with the same names and signatures in every task.
- Placeholders: none.
- Review round 1 (plan) folded in: golden files (Task 2.0), exhaustive literal checklist (Task 2.2), valid `cargo test` filters, process-group + bounded-drain `run_with_timeout` with the orphan test, IP-literal origin validation, active-session `modified`, explicit-first root order with the symlink test, config bounds, the outage tests, the exact negative live check, the trailer on every commit.
