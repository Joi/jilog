//! JilogConfig — TOML-based configuration for the jilog CLI.

use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

use jilog_review::{
    Reader, Tracker,
    readers::{AmplifierReader, ClaudeCodeReader, CodexReader, ContextIntelligenceReader, CopilotReader, GenericReader, PiReader, SessionIdSource},
    trackers::{GithubTracker, KataTracker, NoneTracker},
    util::expand_tilde,
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
                        Box::new(GenericReader::new(name, path, source))
                    }
                }
            })
            .collect()
    }

    /// Build a Tracker implementation from config.
    pub fn into_tracker(&self) -> Box<dyn Tracker> {
        match &self.tracker {
            TrackerConfig::Beads { .. } => {
                unreachable!("tracker=\"beads\" is rejected in JilogConfig::from_toml_str")
            }
            TrackerConfig::Github { repo } => {
                Box::new(GithubTracker::new(repo))
            }
            TrackerConfig::Kata { project } => {
                Box::new(KataTracker::new(project))
            }
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
