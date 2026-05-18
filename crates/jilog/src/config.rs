//! JilogConfig — TOML-based configuration for the jilog CLI.

use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

use jilog_review::{
    Reader, Tracker,
    readers::{AmplifierReader, ClaudeCodeReader, CodexReader, CopilotReader, GenericReader, SessionIdSource},
    trackers::{BeadsTracker, GithubTracker, KataTracker, NoneTracker},
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
    Copilot {
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
    Beads { repo: String },
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
        let cfg: Self = toml::from_str(&raw).with_context(|| "parse jilog.toml")?;
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
                    ReaderConfig::Copilot { path } => {
                        let dir = path
                            .as_deref()
                            .map(expand_tilde)
                            .unwrap_or_else(|| expand_tilde("~/.copilot/session-state"));
                        Box::new(CopilotReader::new(dir))
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
            TrackerConfig::Beads { repo } => {
                Box::new(BeadsTracker::new(expand_tilde(repo)))
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
