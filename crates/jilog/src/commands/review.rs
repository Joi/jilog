//! `jilog review` — session transcript review pipeline.

use anyhow::Context;
use chrono::{Duration, NaiveDate, Utc};

use jilog_review::digest::{DigestReport, ReviewArgs as LibReviewArgs};
use jilog_review::util::{contract_tilde, digest_file_path, expand_tilde};

use crate::config::JilogConfig;

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

#[derive(clap::Args, Debug)]
pub struct ReviewArgs {
    #[command(subcommand)]
    pub subcmd: ReviewSubcmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum ReviewSubcmd {
    /// Run the nightly review pipeline.
    Nightly(NightlyArgs),
}

#[derive(clap::Args, Debug)]
pub struct NightlyArgs {
    /// Look-back window in days (default: 1).
    #[arg(long, default_value_t = 1)]
    pub days: u32,

    /// Time window (e.g. "7d", "24h", "2026-05-10"). Conflicts with --days when both are user-supplied.
    #[arg(long, conflicts_with = "days")]
    pub since: Option<String>,

    /// Emit a single JSON object to stdout instead of the human summary.
    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// Output digest directory (default: from config zone or ~/.jilog/digests).
    #[arg(long)]
    pub digest_dir: Option<std::path::PathBuf>,

    /// Skip file writes and issue creation.
    #[arg(long)]
    pub dry_run: bool,

    /// Create issues in the configured tracker for each detected signal.
    #[arg(long)]
    pub create_issues: bool,

    /// Date stamp for the digest (YYYY-MM-DD). Default: today.
    #[arg(long)]
    pub date: Option<NaiveDate>,

    /// Path to the processed-sessions dedup file.
    #[arg(long)]
    pub processed_file: Option<std::path::PathBuf>,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn run(cfg: &JilogConfig, args: ReviewArgs) -> anyhow::Result<()> {
    match args.subcmd {
        ReviewSubcmd::Nightly(nightly) => run_nightly(cfg, &nightly),
    }
}

fn run_nightly(cfg: &JilogConfig, args: &NightlyArgs) -> anyhow::Result<()> {
    let since = nightly_since(args)?;

    let digest_dir = args
        .digest_dir
        .clone()
        .or_else(|| {
            cfg.zones
                .first()
                .map(|z| expand_tilde(&z.ledger_path).join("digests"))
        })
        .unwrap_or_else(|| expand_tilde("~/.jilog/digests"));

    let date = args.date.unwrap_or_else(|| Utc::now().date_naive());

    let processed_file = args.processed_file.clone().or_else(|| {
        Some(expand_tilde("~/.jilog/telemetry/processed-sessions.txt"))
    });

    let readers = cfg.into_readers();

    // Issue-body backlinks use the REAL digest file with the SAME date the
    // filename uses — one date source, threaded everywhere (jilog#re4k).
    let date_str = date.format("%Y-%m-%d").to_string();
    let digest_display_path =
        contract_tilde(&digest_file_path(&digest_dir, &date_str));
    let tracker = cfg.into_tracker(Some((digest_display_path.as_str(), date_str.as_str())));

    let review_args = LibReviewArgs {
        since,
        digest_dir: digest_dir.clone(),
        processed_file,
        date,
        dry_run: args.dry_run,
        create_issues: args.create_issues,
    };

    let report = jilog_review::run_review(readers.as_slice(), tracker.as_ref(), &review_args)
        .with_context(|| "review pipeline failed")?;

    if args.json {
        let value = digest_report_json(&report, args.dry_run);
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .with_context(|| "failed to serialize review JSON")?
        );
    } else {
        println!(
            "{} corrections, {} errors, {} workarounds, {} deferrals, {} patterns, {} P0 alert(s), {} session(s) scanned",
            report.corrections.len(),
            report.errors.len(),
            report.workarounds.len(),
            report.deferrals.len(),
            report.patterns.len(),
            report.p0_alerts.len(),
            report.sessions_scanned,
        );

        if let Some(sp) = &report.spend {
            if let Some(total) = &sp.total_cost_usd {
                println!(
                    "Spend: ${} across {} of {} session(s) with usage data",
                    total, sp.sessions_with_cost, sp.sessions_with_stats
                );
            }
        }

        if !args.dry_run {
            println!("Digest: {}", report.digest_path.display());
        }

        if !report.created_issues.is_empty() {
            println!("Created {} issue(s)", report.created_issues.len());
        }

        if report.tracker_failures > 0 {
            println!(
                "Tracker failures: {} (affected sessions retry next run)",
                report.tracker_failures
            );
        }
    }

    Ok(())
}

fn nightly_since(args: &NightlyArgs) -> anyhow::Result<chrono::DateTime<Utc>> {
    if let Some(since) = &args.since {
        crate::commands::query::parse_since(since)
            .with_context(|| format!("invalid --since value: {}", since))
    } else {
        Ok(Utc::now() - Duration::days(args.days as i64))
    }
}

fn digest_report_json(report: &DigestReport, dry_run: bool) -> serde_json::Value {
    let mut p0_alerts = serde_json::Map::new();
    for (tool, sessions) in &report.p0_alerts {
        p0_alerts.insert(
            tool.clone(),
            serde_json::Value::Array(
                sessions
                    .iter()
                    .map(|session| serde_json::Value::String(session.clone()))
                    .collect(),
            ),
        );
    }

    let created_issues = report
        .created_issues
        .iter()
        .map(|issue| {
            serde_json::json!({
                "id": &issue.id,
                "backend": &issue.backend,
                "title": &issue.title,
                "url": &issue.url,
            })
        })
        .collect();

    let digest_path = if dry_run {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(report.digest_path.display().to_string())
    };

    // Spend is null when no scanned session carried usage data. Costs are
    // string-decimals (observed values summed with rust_decimal, no floats).
    let spend = match &report.spend {
        None => serde_json::Value::Null,
        Some(sp) => serde_json::json!({
            "total_usd": sp.total_cost_usd.as_ref().map(|d| d.to_string()),
            "sessions_with_stats": sp.sessions_with_stats,
            "sessions_with_cost": sp.sessions_with_cost,
            "input_tokens": sp.input_tokens,
            "output_tokens": sp.output_tokens,
            "role_costs_usd": sp.role_costs.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string())))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
            "model_costs_usd": sp.model_costs.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string())))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        }),
    };

    // Fleet persona rollup: `persona@channel` → sessions + per-kind signal
    // counts. Empty object when only coding sessions were scanned, so
    // existing consumers see one new stable key and nothing else changes.
    let personas = report
        .personas
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect::<serde_json::Map<String, serde_json::Value>>();

    serde_json::json!({
        "schema_version": 1,
        "sessions_scanned": report.sessions_scanned,
        "tracker_failures": report.tracker_failures,
        "corrections": report.corrections.len(),
        "errors": report.errors.len(),
        "workarounds": report.workarounds.len(),
        "deferrals": report.deferrals.len(),
        "patterns": report.patterns.len(),
        "p0_alerts": serde_json::Value::Object(p0_alerts),
        "personas": serde_json::Value::Object(personas),
        "spend": spend,
        "digest_path": digest_path,
        "created_issues": serde_json::Value::Array(created_issues),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use clap::Parser;
    use jilog_review::tracker::IssueRef;
    use std::collections::{BTreeSet, HashMap};
    use std::path::PathBuf;

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(subcommand)]
        cmd: TestCmd,
    }

    #[derive(clap::Subcommand, Debug)]
    enum TestCmd {
        Nightly(NightlyArgs),
    }

    fn nightly_args() -> NightlyArgs {
        NightlyArgs {
            days: 1,
            since: None,
            json: false,
            digest_dir: None,
            dry_run: false,
            create_issues: false,
            date: None,
            processed_file: None,
        }
    }

    fn digest_report() -> DigestReport {
        let mut p0_alerts = HashMap::new();
        let mut sessions = BTreeSet::new();
        sessions.insert("session-a".to_string());
        sessions.insert("session-b".to_string());
        p0_alerts.insert("bash".to_string(), sessions);

        DigestReport {
            date: chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(),
            corrections: Vec::new(),
            errors: Vec::new(),
            workarounds: Vec::new(),
            deferrals: Vec::new(),
            patterns: Vec::new(),
            p0_alerts,
            spend: None,
            personas: std::collections::BTreeMap::from([(
                "jibot@The vibez".to_string(),
                jilog_review::PersonaCounts {
                    persona: "jibot".to_string(),
                    channel: Some("The vibez".to_string()),
                    sessions: 2,
                    corrections: 1,
                    errors: 0,
                    workarounds: 0,
                    deferrals: 0,
                    patterns: 1,
                },
            )]),
            digest_path: PathBuf::from("/tmp/learning-digest-2026-05-10.md"),
            created_issues: vec![IssueRef {
                id: "#42".to_string(),
                backend: "github".to_string(),
                title: "tracked issue".to_string(),
                url: Some("https://example.com/issues/42".to_string()),
            }],
            sessions_scanned: 3,
            tracker_failures: 0,
        }
    }

    #[test]
    fn since_alone_does_not_conflict_with_default_days() {
        let parsed = TestCli::try_parse_from(["test", "nightly", "--since", "24h"]).unwrap();
        let TestCmd::Nightly(args) = parsed.cmd;

        assert_eq!(args.since.as_deref(), Some("24h"));
        assert_eq!(args.days, 1);
    }

    #[test]
    fn since_conflicts_with_user_supplied_days() {
        let err = TestCli::try_parse_from(["test", "nightly", "--since", "24h", "--days", "1"])
            .unwrap_err()
            .to_string();

        assert!(err.contains("--since"));
        assert!(err.contains("--days"));
    }

    #[test]
    fn nightly_since_24h_matches_days_one() {
        let mut args = nightly_args();
        args.since = Some("24h".to_string());

        let cutoff = nightly_since(&args).unwrap();
        let days_cutoff = nightly_since(&nightly_args()).unwrap();
        let delta = cutoff.signed_duration_since(days_cutoff).num_seconds().abs();
        assert!(delta <= 2, "cutoff differed by {delta} seconds");
    }

    #[test]
    fn nightly_since_reports_parse_errors() {
        let mut args = nightly_args();
        args.since = Some("notaduration".to_string());

        let err = nightly_since(&args).unwrap_err().to_string();
        assert!(err.contains("invalid --since value: notaduration"));
    }

    #[test]
    fn json_output_has_documented_keys() {
        let value = digest_report_json(&digest_report(), false);
        let encoded = serde_json::to_string(&value).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let object = parsed.as_object().unwrap();

        let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "schema_version",
                "sessions_scanned",
                "tracker_failures",
                "corrections",
                "errors",
                "workarounds",
                "deferrals",
                "patterns",
                "p0_alerts",
                "personas",
                "spend",
                "digest_path",
                "created_issues",
            ])
        );
        // Populated personas entry: the exact serialized shape is a
        // documented-stable surface — consumers parse the persona/channel
        // FIELDS (the map key is display-only and may be disambiguated).
        assert_eq!(
            parsed["personas"],
            serde_json::json!({
                "jibot@The vibez": {
                    "persona": "jibot",
                    "channel": "The vibez",
                    "sessions": 2,
                    "corrections": 1,
                    "errors": 0,
                    "workarounds": 0,
                    "deferrals": 0,
                    "patterns": 1,
                }
            })
        );
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["sessions_scanned"], 3);
        assert_eq!(
            parsed["p0_alerts"]["bash"],
            serde_json::json!(["session-a", "session-b"])
        );
        assert_eq!(parsed["created_issues"][0]["id"], "#42");
        assert_eq!(parsed["created_issues"][0]["backend"], "github");
        assert_eq!(parsed["created_issues"][0]["title"], "tracked issue");
        assert_eq!(
            parsed["created_issues"][0]["url"],
            "https://example.com/issues/42"
        );
    }

    #[test]
    fn json_dry_run_uses_null_digest_path() {
        let mut report = digest_report();
        report.created_issues.clear();

        let value = digest_report_json(&report, true);

        assert!(value["digest_path"].is_null());
        assert_eq!(value["created_issues"], serde_json::json!([]));
    }

    #[test]
    fn parse_since_accepts_iso_dates() {
        let cutoff = crate::commands::query::parse_since("2026-05-10").unwrap();

        assert_eq!(cutoff, Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap());
    }
}
