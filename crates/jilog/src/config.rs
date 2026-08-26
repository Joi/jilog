//! JilogConfig — TOML-based configuration for the jilog CLI.

use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

use jilog_review::{
    Reader, Tracker,
    readers::{AmplifierReader, ClaudeCodeReader, CodexReader, ContextIntelligenceReader, CopilotReader, GenericReader, NanoclawReader, PiReader, SessionIdSource},
    trackers::{GithubTracker, KataTracker, NoneTracker},
    util::{expand_tilde, expand_tilde_glob},
};

// ---------------------------------------------------------------------------
// Config schema
// ---------------------------------------------------------------------------

/// Root configuration object (parsed from jilog.toml).
#[derive(Debug, Deserialize, Default)]
pub struct JilogConfig {
    #[serde(default, rename = "reader")]
    pub readers: Vec<ReaderConfig>,
    #[serde(default)]
    pub tracker: TrackerConfig,
    #[serde(default, rename = "zone")]
    pub zones: Vec<ZoneConfig>,
}

/// A configured reader.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ReaderConfig {
    Amplifier {
        #[serde(default)]
        path: Option<String>,
    },
    ClaudeCode {
        #[serde(default)]
        path: Option<String>,
    },
    Codex {
        #[serde(default)]
        path: Option<String>,
    },
    /// Amplifier context-intelligence event streams
    /// (`<projects>/<proj>/sessions/<sess>/context-intelligence/events.jsonl`).
    ContextIntelligence {
        #[serde(default)]
        path: Option<String>,
    },
    Copilot {
        #[serde(default)]
        path: Option<String>,
    },
    /// NanoClaw cell agent sessions
    /// (`<path>/v2-sessions/<agent-id>/.claude-shared/projects/**/*.jsonl`,
    /// persona/channel mapped via `<path>/v2.db`). `include`/`exclude` are
    /// the per-cell trust-tier allowlist, matched against agent id, persona,
    /// and folder; exclude wins.
    Nanoclaw {
        #[serde(default)]
        path: Option<String>,
        /// Routing db override; defaults to `<path>/v2.db`.
        #[serde(default)]
        db: Option<String>,
        #[serde(default)]
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// pi coding agent (pi.dev) session files
    /// (`~/.pi/agent/sessions/<project-slug>/<timestamp>_<uuid>.jsonl`).
    Pi {
        #[serde(default)]
        path: Option<String>,
    },
    Generic {
        name: String,
        path: String,
        #[serde(default)]
        session_id_from: GenericSessionIdSource,
    },
}

/// Session-ID derivation strategy for the generic reader.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GenericSessionIdSource {
    #[default]
    ParentDir,
    FileStem,
}

/// A configured tracker backend.
#[derive(Debug, Deserialize, Default)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum TrackerConfig {
    /// Removed backend (beads is deprecated). Still parsed so that configs
    /// naming it fail at load with an error listing the remaining options,
    /// instead of a generic unknown-variant message.
    Beads {
        #[serde(default)]
        #[allow(dead_code)]
        repo: Option<String>,
    },
    Github { repo: String },
    /// kata local-first tracker. `project` is the kata project name
    /// (created via `kata init --project <name>` in a workspace dir).
    Kata { project: String },
    #[default]
    None,
}

/// A named ledger zone (path to the segment store).
#[derive(Debug, Deserialize)]
pub struct ZoneConfig {
    pub id: String,
    pub ledger_path: String,
    /// Whether this zone participates in spool replication (default
    /// true). Set `spool = false` on read-only mirror zones — e.g. the
    /// fleet store mounted as a `[[zone]]` on every machine — so
    /// `spool emit` never re-emits an already-replicated store into an
    /// orphan spool.
    #[serde(default = "default_true")]
    pub spool: bool,
    /// Spool root for cross-machine replication (jilog#546w). Default:
    /// a `spool/<zone-id>` sibling of `ledger_path` — inside the same
    /// Syncthing-synced tree, so replication transport comes free.
    #[serde(default)]
    pub spool_path: Option<String>,
    /// AUTHORITY ONLY: the fleet store `spool ingest` commits into. The
    /// single host that sets this (jibotmac) is the fleet store's only
    /// writer; every other machine leaves it unset and only emits.
    #[serde(default)]
    pub fleet_store_path: Option<String>,
    /// Where this zone's SQLite index lives (jilog#546w round 2).
    /// Default: `<ledger_path>/index.sqlite` for normal (spool = true)
    /// zones — compatible with opsctl-maintained indexes — but
    /// `~/.jilog/index/<zone-id>.sqlite` for `spool = false` mirror
    /// zones, because their ledger directory is a SYNCED tree and a
    /// SQLite file must never live inside one (a background sync of a
    /// mid-transaction db ships corruption). Set this to override.
    #[serde(default)]
    pub index_path: Option<String>,
}

impl ZoneConfig {
    /// Resolve the SQLite index location for this zone (see
    /// `index_path` for the default rules).
    pub fn index_db_path(&self) -> PathBuf {
        if let Some(p) = &self.index_path {
            return expand_tilde(p);
        }
        if self.spool {
            expand_tilde(&self.ledger_path).join("index.sqlite")
        } else {
            expand_tilde(&format!("~/.jilog/index/{}.sqlite", self.id))
        }
    }
}

/// Serde default helper: `ZoneConfig::spool` defaults to true.
fn default_true() -> bool {
    true
}

/// Lexically normalize a path: drop `.` components and resolve `..`
/// against the preceding component, WITHOUT touching the filesystem.
/// Used by config validation, where the paths may not exist yet.
/// (Symlinks are deliberately not resolved — lexical only.)
fn normalize_lexical(p: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop the previous component; at the root (or with
                // nothing left to pop) keep the `..` so the result
                // stays an honest over-approximation.
                let popped = matches!(
                    out.components().next_back(),
                    Some(Component::Normal(_))
                ) && out.pop();
                if !popped {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// JilogConfig methods
// ---------------------------------------------------------------------------

impl JilogConfig {
    /// Load config from `path`.
    pub fn load(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("read config at {}", path.as_ref().display()))?;
        Self::from_toml_str(&raw)
    }

    /// Parse and validate config from a TOML string.
    pub fn from_toml_str(raw: &str) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(raw).with_context(|| "parse jilog.toml")?;
        if matches!(cfg.tracker, TrackerConfig::Beads { .. }) {
            anyhow::bail!(
                "tracker type \"beads\" was removed in jilog 0.2.0 (beads is deprecated); \
                 set [tracker] type to one of: \"kata\", \"github\", \"none\""
            );
        }
        // Invariant: an explicit index_path must never sit inside the
        // zone's ledger tree. The ledger tree may be file-synced
        // (Syncthing), and syncing a mid-transaction SQLite db ships
        // corruption to every other machine. The check is LEXICAL on
        // the tilde-expanded, `.`/`..`-normalized paths (no filesystem
        // access at config-validation time); a symlink ancestor that
        // re-enters the ledger tree is out of scope here — the runtime
        // hazard note stands regardless. The spool=true DEFAULT index
        // location is exempt for opsctl compatibility — it predates
        // syncing.
        for z in &cfg.zones {
            if let Some(ip) = &z.index_path {
                let idx = normalize_lexical(&expand_tilde(ip));
                let ledger = normalize_lexical(&expand_tilde(&z.ledger_path));
                if idx == ledger || idx.starts_with(&ledger) {
                    anyhow::bail!(
                        "zone {:?}: index_path {} is inside ledger_path {} — a SQLite \
                         index must never live in the (possibly synced) ledger tree; \
                         a background sync of a mid-transaction db ships corruption. \
                         Point index_path at local disk, e.g. ~/.jilog/index/{}.sqlite",
                        z.id,
                        idx.display(),
                        ledger.display(),
                        z.id
                    );
                }
            }
        }
        Ok(cfg)
    }

    /// Return the default config search path:
    /// `$JILOG_CONFIG` → `~/.jilog.toml` → `./jilog.toml`.
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(env_path) = std::env::var("JILOG_CONFIG") {
            return Some(PathBuf::from(env_path));
        }
        let home = expand_tilde("~/.jilog.toml");
        if home.exists() {
            return Some(home);
        }
        let local = PathBuf::from("jilog.toml");
        if local.exists() {
            return Some(local);
        }
        None
    }

    /// Build Reader implementations from config.
    pub fn into_readers(&self) -> Vec<Box<dyn Reader>> {
        if self.readers.is_empty() {
            // Default: Amplifier reader.
            return vec![Box::new(AmplifierReader::from_default())];
        }
        self.readers
            .iter()
            .map(|rc| -> Box<dyn Reader> {
                match rc {
                    ReaderConfig::Amplifier { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.amplifier/projects"));
                        Box::new(AmplifierReader::new(dir))
                    }
                    ReaderConfig::ClaudeCode { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.claude/projects"));
                        Box::new(ClaudeCodeReader::new(dir))
                    }
                    ReaderConfig::Codex { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.codex/sessions"));
                        Box::new(CodexReader::new(dir))
                    }
                    ReaderConfig::ContextIntelligence { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.amplifier/projects"));
                        Box::new(ContextIntelligenceReader::new(dir))
                    }
                    ReaderConfig::Copilot { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.copilot/session-state"));
                        Box::new(CopilotReader::new(dir))
                    }
                    ReaderConfig::Nanoclaw { path, db, include, exclude } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/nanoclaw/data"));
                        let mut reader = NanoclawReader::new(dir)
                            .with_allowlist(include.clone(), exclude.clone());
                        if let Some(db) = db {
                            reader = reader.with_db_path(expand_tilde(db));
                        }
                        Box::new(reader)
                    }
                    ReaderConfig::Pi { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.pi/agent/sessions"));
                        Box::new(PiReader::new(dir))
                    }
                    ReaderConfig::Generic { name, path, session_id_from } => {
                        let source = match session_id_from {
                            GenericSessionIdSource::ParentDir => SessionIdSource::ParentDir,
                            GenericSessionIdSource::FileStem => SessionIdSource::FileStem,
                        };
                        // Glob pattern, but `~` still means the home dir
                        // like every other reader's `path`. The expanded
                        // home is ESCAPED: `path` is spliced into a glob,
                        // so metacharacters in $HOME must match literally.
                        let pattern = expand_tilde_glob(path);
                        Box::new(GenericReader::new(name, pattern, source))
                    }
                }
            })
            .collect()
    }

    /// Build a Tracker implementation from config.
    ///
    /// `run_context`, when present, is `(digest_path, date)` for the run —
    /// the REAL digest file the run writes and the SAME date string used in
    /// its filename — so issue bodies backlink correctly on hosts whose
    /// `--digest-dir` deviates from the default and never mix date sources
    /// (jilog#re4k). Only the kata tracker uses it today.
    pub fn into_tracker(&self, run_context: Option<(&str, &str)>) -> Box<dyn Tracker> {
        match &self.tracker {
            TrackerConfig::Beads { .. } => {
                unreachable!("tracker=\"beads\" is rejected in JilogConfig::from_toml_str")
            }
            TrackerConfig::Github { repo } => {
                Box::new(GithubTracker::new(repo))
            }
            TrackerConfig::Kata { project } => match run_context {
                Some((digest_path, date)) => {
                    Box::new(KataTracker::with_run_context(project, digest_path, date))
                }
                None => Box::new(KataTracker::new(project)),
            },
            TrackerConfig::None => Box::new(NoneTracker),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beads_tracker_rejected_with_loud_error() {
        // With and without the old `repo` field — both must fail the same way.
        for raw in [
            "[tracker]\ntype = \"beads\"\nrepo = \"~/ops\"\n",
            "[tracker]\ntype = \"beads\"\n",
        ] {
            let err = JilogConfig::from_toml_str(raw)
                .expect_err("beads config must be rejected")
                .to_string();
            assert!(err.contains("beads"), "error names beads: {err}");
            assert!(err.contains("\"kata\""), "error names kata: {err}");
            assert!(err.contains("\"github\""), "error names github: {err}");
            assert!(err.contains("\"none\""), "error names none: {err}");
        }
    }

    #[test]
    fn pi_reader_parses_with_and_without_path() {
        let cfg = JilogConfig::from_toml_str(
            "[[reader]]\ntype = \"pi\"\npath = \"~/.pi/agent/sessions\"\n",
        )
        .unwrap();
        assert!(matches!(cfg.readers[0], ReaderConfig::Pi { .. }));
        assert_eq!(cfg.into_readers().len(), 1);

        let cfg = JilogConfig::from_toml_str("[[reader]]\ntype = \"pi\"\n").unwrap();
        assert!(matches!(cfg.readers[0], ReaderConfig::Pi { path: None }));
        assert_eq!(cfg.into_readers()[0].name(), "pi");
    }

    #[test]
    fn generic_reader_expands_tilde_in_path() {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Hermetic: a scratch dir under the real $HOME whose name is unique
        // per (pid, nanos, counter), created with create_dir (NOT _all) so it
        // MUST NOT pre-exist — we only ever remove a dir we just created, so a
        // PID collision can't delete someone else's directory.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let home = std::env::var("HOME").expect("HOME set in tests");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let scratch = format!(
            ".jilog-test-generic-{}-{}-{}",
            std::process::id(),
            nanos,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::path::Path::new(&home).join(&scratch);
        std::fs::create_dir(&dir).expect("unique scratch dir must not pre-exist");
        // RAII cleanup: the name is unique per run, so a panic below would
        // otherwise leak an unreclaimable directory into the real home.
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Scratch(dir.clone());
        std::fs::write(dir.join("s1.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();

        let cfg = JilogConfig::from_toml_str(&format!(
            "[[reader]]\ntype = \"generic\"\nname = \"hermes\"\npath = \"~/{}/*.jsonl\"\nsession_id_from = \"file_stem\"\n",
            scratch
        ))
        .unwrap();
        let readers = cfg.into_readers();
        let handles = readers[0]
            .discover({ use chrono::TimeZone; chrono::Utc.timestamp_opt(0, 0).single().unwrap() })
            .unwrap();
        assert_eq!(handles.len(), 1, "~ must expand for the generic reader's glob");
        assert_eq!(handles[0].session_id, "s1");
    }

    #[test]
    fn nanoclaw_reader_parses_allowlist() {
        let cfg = JilogConfig::from_toml_str(
            "[[reader]]\ntype = \"nanoclaw\"\npath = \"~/nanoclaw/data\"\ninclude = [\"jibot\", \"canary\"]\nexclude = [\"bifbot\", \"bif-2027-steering\"]\n",
        )
        .unwrap();
        match &cfg.readers[0] {
            ReaderConfig::Nanoclaw { include, exclude, db, .. } => {
                assert_eq!(include, &["jibot", "canary"]);
                assert_eq!(exclude, &["bifbot", "bif-2027-steering"]);
                assert!(db.is_none());
            }
            other => panic!("expected nanoclaw reader, got {:?}", other),
        }
        assert_eq!(cfg.into_readers()[0].name(), "nanoclaw");

        // Bare form: everything defaults.
        let cfg = JilogConfig::from_toml_str("[[reader]]\ntype = \"nanoclaw\"\n").unwrap();
        assert!(matches!(cfg.readers[0], ReaderConfig::Nanoclaw { .. }));
        assert_eq!(cfg.into_readers().len(), 1);
    }

    #[test]
    fn zone_spool_flag_defaults_true() {
        let cfg = JilogConfig::from_toml_str(
            "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\n",
        )
        .unwrap();
        assert!(cfg.zones[0].spool, "spool must default to true");

        let cfg = JilogConfig::from_toml_str(
            "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\nspool = false\n",
        )
        .unwrap();
        assert!(!cfg.zones[0].spool);
    }

    #[test]
    fn zone_index_path_inside_ledger_tree_is_rejected() {
        // Directly inside the ledger tree: rejected — including paths
        // that only reach it through `..` traversal.
        for bad in [
            "/tmp/l/index.sqlite",
            "/tmp/l/sub/idx.sqlite",
            "/tmp/l",
            "/tmp/other/../l/index.sqlite",
            "/tmp/l/./index.sqlite",
        ] {
            let err = JilogConfig::from_toml_str(&format!(
                "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\nindex_path = \"{bad}\"\n"
            ))
            .expect_err("index_path inside ledger_path must be rejected")
            .to_string();
            assert!(err.contains("inside ledger_path"), "{bad}: {err}");
            assert!(err.contains("corruption"), "{bad}: {err}");
        }

        // Outside the tree (including a sibling with a shared name
        // prefix, and a `..` path that lands outside): allowed.
        for good in [
            "/tmp/l-index/z.sqlite",
            "/var/idx/z.sqlite",
            "/tmp/l/../l-index/z.sqlite",
        ] {
            JilogConfig::from_toml_str(&format!(
                "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\nindex_path = \"{good}\"\n"
            ))
            .unwrap_or_else(|e| panic!("{good} should be allowed: {e}"));
        }
    }

    #[test]
    fn zone_index_path_resolution() {
        // Normal zone: index lives beside the segments (opsctl-compatible).
        let cfg = JilogConfig::from_toml_str(
            "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.zones[0].index_db_path(),
            std::path::PathBuf::from("/tmp/l/index.sqlite")
        );

        // Mirror zone (spool = false): index defaults OUT of the synced
        // tree, into the local ~/.jilog/index/.
        let cfg = JilogConfig::from_toml_str(
            "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\nspool = false\n",
        )
        .unwrap();
        let p = cfg.zones[0].index_db_path();
        assert!(
            p.ends_with(".jilog/index/z.sqlite"),
            "mirror index must default outside the ledger tree: {}",
            p.display()
        );

        // Explicit index_path always wins.
        let cfg = JilogConfig::from_toml_str(
            "[[zone]]\nid = \"z\"\nledger_path = \"/tmp/l\"\nspool = false\nindex_path = \"/var/idx/z.sqlite\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.zones[0].index_db_path(),
            std::path::PathBuf::from("/var/idx/z.sqlite")
        );
    }

    #[test]
    fn remaining_trackers_still_parse() {
        let cfg = JilogConfig::from_toml_str(
            "[tracker]\ntype = \"kata\"\nproject = \"jilog\"\n",
        )
        .unwrap();
        assert!(matches!(cfg.tracker, TrackerConfig::Kata { .. }));

        let cfg = JilogConfig::from_toml_str(
            "[tracker]\ntype = \"github\"\nrepo = \"o/r\"\n",
        )
        .unwrap();
        assert!(matches!(cfg.tracker, TrackerConfig::Github { .. }));

        let cfg = JilogConfig::from_toml_str("").unwrap();
        assert!(matches!(cfg.tracker, TrackerConfig::None));
    }
}
