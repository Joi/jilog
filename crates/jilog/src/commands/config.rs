//! `jilog config check` — report what a config feeds and warn on raw-signal
//! filing (jilog#1xmf).

use crate::config::{JilogConfig, ReaderConfig, TrackerConfig};

#[derive(clap::Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub cmd: ConfigCmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum ConfigCmd {
    /// Show which readers feed the errors and patterns detectors, and warn
    /// (exit 1) when a tracker files every raw signal as an issue.
    Check,
}

/// What a reader can give the detectors beyond user/assistant messages.
struct Feeds {
    /// `role: "tool"` results — the input of `detect_errors`. `Some(true)`
    /// always, `Some(false)` never, `None` only if its files hold them.
    errors: Option<bool>,
    /// An event stream from `load_events` — the input of
    /// `detect_health_patterns`.
    patterns: bool,
}

/// The reader table from the jilog#1xmf design, one arm per variant and no
/// wildcard: a new reader type does not compile until it is classified.
fn feeds(reader: &ReaderConfig) -> (&'static str, Feeds) {
    match reader {
        ReaderConfig::Amplifier { .. } => ("amplifier", Feeds { errors: Some(true), patterns: true }),
        ReaderConfig::ContextIntelligence { .. } => {
            ("context-intelligence", Feeds { errors: Some(true), patterns: true })
        }
        ReaderConfig::Pi { .. } => ("pi", Feeds { errors: Some(true), patterns: true }),
        ReaderConfig::Nanoclaw { .. } => ("nanoclaw", Feeds { errors: Some(true), patterns: true }),
        ReaderConfig::WorkerSignals => ("worker-signals", Feeds { errors: Some(true), patterns: false }),
        ReaderConfig::Generic { .. } => ("generic", Feeds { errors: None, patterns: false }),
        // Claude Code tool results are user-role tool_result blocks. (Its
        // legacy flat form would pass a synthetic `role: "tool"` line
        // through, but real Claude Code transcripts hold none.)
        ReaderConfig::ClaudeCode { .. } => ("claude-code", Feeds { errors: Some(false), patterns: false }),
        // The codex reader drops tool calls.
        ReaderConfig::Codex { .. } => ("codex", Feeds { errors: Some(false), patterns: false }),
        ReaderConfig::Copilot { .. } => ("copilot", Feeds { errors: Some(false), patterns: false }),
        // The archive's non user/assistant rows are skipped.
        ReaderConfig::Agentsview { .. } => ("agentsview", Feeds { errors: Some(false), patterns: false }),
    }
}

/// The lines `config check` prints, and whether any is a warning.
pub fn check_lines(cfg: &JilogConfig) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let default_reader = [ReaderConfig::Amplifier { path: None }];
    let readers: &[ReaderConfig] = if cfg.readers.is_empty() {
        lines.push("readers: none configured — the default amplifier reader is used".to_string());
        &default_reader
    } else {
        &cfg.readers
    };

    let mut errors_always: Vec<&str> = Vec::new();
    let mut errors_maybe: Vec<&str> = Vec::new();
    let mut patterns: Vec<&str> = Vec::new();
    for r in readers {
        let (name, f) = feeds(r);
        match f.errors {
            Some(true) => push_unique(&mut errors_always, name),
            None => push_unique(&mut errors_maybe, name),
            Some(false) => {}
        }
        if f.patterns {
            push_unique(&mut patterns, name);
        }
    }

    let maybe = format!(
        "may be fed by {} (if its files hold tool-role lines)",
        errors_maybe.join(", ")
    );
    lines.push(match (errors_always.is_empty(), errors_maybe.is_empty()) {
        (true, true) => "errors detector: no configured reader feeds it (claude-code, codex, copilot and agentsview carry no tool-role results)".to_string(),
        (true, false) => format!("errors detector: {}", maybe),
        (false, true) => format!("errors detector: fed by {}", errors_always.join(", ")),
        (false, false) => format!("errors detector: fed by {}; {}", errors_always.join(", "), maybe),
    });
    lines.push(if patterns.is_empty() {
        "patterns detector: no configured reader feeds it (only amplifier, context-intelligence, pi and nanoclaw carry an event stream)".to_string()
    } else {
        format!("patterns detector: fed by {}", patterns.join(", "))
    });

    let warned = match &cfg.tracker {
        TrackerConfig::Kata { .. } | TrackerConfig::Github { .. } => {
            let kind = if matches!(cfg.tracker, TrackerConfig::Kata { .. }) { "kata" } else { "github" };
            lines.push(format!(
                "warn: tracker {} files every raw signal as an issue; the recommended fleet config is [tracker] type = \"none\" (the weekly synthesis files issues)",
                kind
            ));
            true
        }
        _ => {
            lines.push("tracker: none (raw signals are not filed)".to_string());
            false
        }
    };
    (lines, warned)
}

fn push_unique<'a>(v: &mut Vec<&'a str>, name: &'a str) {
    if !v.contains(&name) {
        v.push(name);
    }
}

/// Print the check; the process exits 1 when a warning was printed.
pub fn run(cfg: &JilogConfig, args: ConfigArgs) -> anyhow::Result<()> {
    match args.cmd {
        ConfigCmd::Check => {
            let (lines, warned) = check_lines(cfg);
            for line in lines {
                println!("{}", line);
            }
            if warned {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml: &str) -> JilogConfig {
        JilogConfig::from_toml_str(toml).unwrap()
    }

    #[test]
    fn every_reader_type_is_classified() {
        for (ty, errors, patterns) in [
            ("amplifier", "fed by amplifier", "fed by amplifier"),
            ("context-intelligence", "fed by context-intelligence", "fed by context-intelligence"),
            ("pi", "fed by pi", "fed by pi"),
            ("nanoclaw", "fed by nanoclaw", "fed by nanoclaw"),
            ("worker-signals", "fed by worker-signals", "no configured reader"),
            ("claude-code", "no configured reader", "no configured reader"),
            ("codex", "no configured reader", "no configured reader"),
            ("copilot", "no configured reader", "no configured reader"),
            ("agentsview", "no configured reader", "no configured reader"),
        ] {
            let (lines, _) = check_lines(&cfg(&format!("[[reader]]\ntype = \"{ty}\"\n")));
            assert!(lines[0].starts_with("errors detector:") && lines[0].contains(errors), "{ty}: {lines:?}");
            assert!(lines[1].starts_with("patterns detector:") && lines[1].contains(patterns), "{ty}: {lines:?}");
        }
        let (lines, _) = check_lines(&cfg("[[reader]]\ntype = \"generic\"\nname = \"g\"\npath = \"/tmp/x\"\n"));
        assert_eq!(lines[0], "errors detector: may be fed by generic (if its files hold tool-role lines)");
        assert!(lines[1].contains("no configured reader"), "{lines:?}");
    }

    #[test]
    fn guarded_job_readers_feed_neither_detector() {
        let (lines, warned) = check_lines(&cfg(
            "[[reader]]\ntype = \"claude-code\"\n[[reader]]\ntype = \"codex\"\n[[reader]]\ntype = \"agentsview\"\n[tracker]\ntype = \"none\"\n",
        ));
        assert!(lines[0].contains("no configured reader feeds it"), "{lines:?}");
        assert!(lines[1].contains("no configured reader feeds it"), "{lines:?}");
        assert!(!warned);
        assert!(lines.iter().any(|l| l.starts_with("tracker: none")));
    }

    #[test]
    fn mixed_readers_list_the_feeders() {
        let (lines, _) = check_lines(&cfg("[[reader]]\ntype = \"claude-code\"\n[[reader]]\ntype = \"pi\"\n"));
        assert_eq!(lines[0], "errors detector: fed by pi");
        assert_eq!(lines[1], "patterns detector: fed by pi");
    }

    #[test]
    fn empty_readers_mean_the_default_amplifier_reader() {
        let (lines, _) = check_lines(&cfg(""));
        assert!(lines[0].starts_with("readers: none configured"), "{lines:?}");
        assert_eq!(lines[1], "errors detector: fed by amplifier");
        assert_eq!(lines[2], "patterns detector: fed by amplifier");
    }

    #[test]
    fn kata_and_github_trackers_warn() {
        let (lines, warned) = check_lines(&cfg("[tracker]\ntype = \"kata\"\nproject = \"jilog\"\n"));
        assert!(warned);
        assert!(lines.iter().any(|l| l.starts_with("warn: tracker kata")), "{lines:?}");
        let (_, warned) = check_lines(&cfg("[tracker]\ntype = \"github\"\nrepo = \"o/r\"\n"));
        assert!(warned);
        let (_, warned) = check_lines(&cfg("[tracker]\ntype = \"none\"\n"));
        assert!(!warned);
    }
}
