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

    let (date, zone) = nightly_date(cfg, args, Utc::now())?;

    let processed_file = args.processed_file.clone().or_else(|| {
        Some(expand_tilde("~/.jilog/telemetry/processed-sessions.txt"))
    });

    let readers = cfg.into_readers();

    // Issue-body backlinks use the REAL digest file with the SAME date the
    // filename uses — one date source, threaded everywhere (jilog#re4k).
    // Absolutize first: a relative --digest-dir would otherwise put a
    // cwd-dependent path into issue bodies read long after the run.
    let date_str = date.format("%Y-%m-%d").to_string();
    let digest_dir_abs = if digest_dir.is_absolute() {
        digest_dir.clone()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&digest_dir))
            .unwrap_or_else(|_| digest_dir.clone())
    };
    let digest_display_path =
        contract_tilde(&digest_file_path(&digest_dir_abs, &date_str));
    let tracker = cfg.into_tracker(Some((digest_display_path.as_str(), date_str.as_str())));

    // Archive spend (agentsview): advisory. Any failure hides the block
    // and the run continues.
    let archive_spend = cfg
        .agentsview_settings()
        .and_then(|s| load_archive_spend(&s, date, zone.name()));

    let review_args = LibReviewArgs {
        since,
        digest_dir: digest_dir.clone(),
        processed_file,
        date,
        dry_run: args.dry_run,
        create_issues: args.create_issues,
        archive_spend,
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

        if let Some(a) = &report.archive_spend {
            println!(
                "Archive spend: {} yesterday, ${} trailing 7d ({} day(s))",
                a.yesterday
                    .as_ref()
                    .map(|y| format!("${}", y.total_usd))
                    .unwrap_or_else(|| "n/a".into()),
                a.week.total_usd,
                a.week.days
            );
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

/// The digest's archive Spend block, or None. Advisory by contract: the
/// daemon is probed first (a `usage daily` run answers from the local
/// archive even when the daemon is down, which would render stale numbers
/// the brief says to hide), then the CLI runs under the timeout. Every
/// failure — unreachable, no binary, mid-sync block, timeout, bad JSON,
/// empty window — logs one warning and hides the block; the run goes on.
pub fn load_archive_spend(
    settings: &crate::config::AgentsviewSettings,
    date: NaiveDate,
    timezone: &str,
) -> Option<jilog_review::ArchiveSpend> {
    let reader = match jilog_review::readers::AgentsviewReader::new(
        &settings.url,
        settings.token_file.clone(),
        settings.since_days,
        settings.timeout,
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
    match jilog_review::archive_spend::fetch_daily_usage(&settings.bin, from, to, timezone, settings.timeout) {
        Ok(rows) => {
            let spend = jilog_review::ArchiveSpend::summarize(&rows, date, timezone);
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

/// The digest date and the zone it is dated in. `--date` wins; otherwise
/// `now` in the resolved zone (`JILOG_TZ`, jilog.toml `timezone`, `TZ`,
/// the system zone, then UTC with a warning) — never a bare UTC date: the
/// 22:50 nightly must call the day it ran "today", and the morning brief's
/// "yesterday" must be the same calendar day the archive block calls
/// yesterday (jilog#0qpq, amplifier-bundle-joi#rvbm). The zone also goes
/// to agentsview as `--timezone`, so the window and the buckets agree.
fn nightly_date(
    cfg: &JilogConfig,
    args: &NightlyArgs,
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<(NaiveDate, chrono_tz::Tz)> {
    let (zone, zone_source) = jilog_review::zone::resolve_zone(cfg.timezone.as_deref())
        .with_context(|| "resolve the digest time zone")?;
    if zone_source == jilog_review::zone::ZoneSource::Fallback {
        tracing::warn!("no time zone from JILOG_TZ, jilog.toml, TZ or the system: dating the digest in UTC");
    }
    let date = args
        .date
        .unwrap_or_else(|| jilog_review::zone::local_date(now, zone));
    Ok((date, zone))
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

    let mut value = serde_json::json!({
        "schema_version": 2,
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
    });

    // Archive spend (agentsview): present only when the block rendered, so
    // an unconfigured host's document keeps today's exact key set (the
    // golden and key-set tests guard it). Costs are decimal strings.
    if let Some(a) = &report.archive_spend {
        let period = |p: &jilog_review::PeriodSpend| {
            serde_json::json!({
                "total_usd": p.total_usd.to_string(),
                "days": p.days,
                "agents_usd": p.agents.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string())))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
                "models_usd": p.models.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string())))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
            })
        };
        value["archive_spend"] = serde_json::json!({
            "yesterday": a.yesterday.as_ref().map(period),
            "week": period(&a.week),
            "week_from": a.week_from.to_string(),
            "week_to": a.week_to.to_string(),
            "timezone": a.timezone,
        });
    }
    value
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
            archive_spend: None,
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
                    input_tokens: 5000,
                    output_tokens: 250,
                    cost_usd: None,
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

    /// One-shot loopback daemon stand-in: answers every request 200 with
    /// `body`, then stops. Enough for the probe.
    fn one_shot_daemon(body: &'static str) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        base
    }

    #[test]
    fn nightly_survives_an_unreachable_agentsview() {
        // The agentsview reader points at a listener that drops every
        // connection (deterministic, unlike a freed ephemeral port another
        // process could claim): the probe fails, the block is hidden, and
        // a full (non-dry) run still writes a digest and exits Ok.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
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
        assert!(load_archive_spend(&settings, date, "UTC").is_none(), "probe fails → hidden");
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
    fn nightly_date_without_date_flag_is_the_local_calendar_day() {
        // JILOG_TZ would outrank the config under test; the process env is
        // not part of this test.
        std::env::remove_var(jilog_review::zone::ENV_OVERRIDE);
        let now = Utc.with_ymd_and_hms(2026, 9, 17, 22, 30, 0).unwrap();
        let args = nightly_args();
        let thimphu = JilogConfig::from_toml_str("timezone = \"Asia/Thimphu\"\n").unwrap();
        let (date, zone) = nightly_date(&thimphu, &args, now).unwrap();
        assert_eq!(date, chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap());
        assert_eq!(zone, chrono_tz::Asia::Thimphu);
        assert_eq!(
            jilog_review::ArchiveSpend::window(date),
            (chrono::NaiveDate::from_ymd_opt(2026, 9, 11).unwrap(), chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap())
        );
        let utc = JilogConfig::from_toml_str("timezone = \"UTC\"\n").unwrap();
        let (date, _) = nightly_date(&utc, &args, now).unwrap();
        assert_eq!(date, chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap());
        // An explicit --date is used as given, whatever the zone says.
        let mut pinned = nightly_args();
        pinned.date = Some(chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        let (date, zone) = nightly_date(&thimphu, &pinned, now).unwrap();
        assert_eq!(date, chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        assert_eq!(zone, chrono_tz::Asia::Thimphu);
    }

    #[cfg(unix)]
    #[test]
    fn nightly_threads_the_zone_and_local_date_into_the_agentsview_call() {
        // The whole path, no --date: the configured zone dates the digest,
        // the window is derived from that date, and the same zone reaches
        // agentsview as --timezone. The stub binary records its argv.
        use std::os::unix::fs::PermissionsExt;
        std::env::remove_var(jilog_review::zone::ENV_OVERRIDE);
        let base = one_shot_daemon(r#"{"machines":[],"machine_labels":{},"machine_aliases":{}}"#);
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, "t").unwrap();
        let calls = dir.path().join("calls");
        let stub = dir.path().join("agentsview");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\necho '{{\"schema_version\":6,\"daily\":[]}}'\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Tokyo, not the host zone: the argv must come from the config.
        let cfg = JilogConfig::from_toml_str(&format!(
            "timezone = \"Asia/Tokyo\"\n[[reader]]\ntype = \"agentsview\"\nurl = \"{base}\"\ntoken_file = \"{}\"\ntimeout_secs = 2\nbin = \"{}\"\n",
            token.display(),
            stub.display()
        ))
        .unwrap();
        let mut args = nightly_args();
        args.dry_run = true;
        args.digest_dir = Some(dir.path().join("digests"));
        args.processed_file = Some(dir.path().join("processed.txt"));
        let before = jilog_review::zone::local_date(Utc::now(), chrono_tz::Asia::Tokyo);
        run_nightly(&cfg, &args).expect("dry run with an empty archive window");
        let after = jilog_review::zone::local_date(Utc::now(), chrono_tz::Asia::Tokyo);
        let argv = std::fs::read_to_string(&calls).expect("agentsview was called");
        let expected = |d: chrono::NaiveDate| {
            let (from, to) = jilog_review::ArchiveSpend::window(d);
            format!("usage daily --json --breakdown --since {from} --until {to} --timezone Asia/Tokyo --no-sync")
        };
        // Tokyo midnight may pass mid-test; either day is right.
        assert!(
            argv.trim() == expected(before) || argv.trim() == expected(after),
            "argv {argv:?} vs {:?}",
            expected(before)
        );
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
        assert!(load_archive_spend(&settings, chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(), "UTC").is_none());
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
            timezone: "Asia/Thimphu".into(),
        });
        let value = digest_report_json(&report, false);
        assert_eq!(value["archive_spend"]["week"]["total_usd"], "2101.500000");
        assert_eq!(value["archive_spend"]["week"]["days"], 7);
        assert_eq!(value["archive_spend"]["week"]["agents_usd"]["codex"], "1300.250000");
        assert_eq!(value["archive_spend"]["week_from"], "2026-09-09");
        assert_eq!(value["archive_spend"]["timezone"], "Asia/Thimphu");
        assert!(value["archive_spend"]["yesterday"].is_null());
        assert_eq!(value["schema_version"], 2);
    }

    #[test]
    fn review_json_bytes_match_golden() {
        // Byte-identity guard (jilog#heyg lens): the full --json document for
        // a host without agentsview. Generated once on the pre-change code
        // with UPDATE_GOLDEN=1; compared byte-for-byte afterwards.
        let value = digest_report_json(&digest_report(), false);
        let got = serde_json::to_string_pretty(&value).unwrap() + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/review-nightly.json");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &got).unwrap();
            panic!(
                "golden regenerated at {} — re-run without UPDATE_GOLDEN to compare",
                path.display()
            );
        }
        let want = std::fs::read_to_string(&path)
            .expect("golden missing — generate once with UPDATE_GOLDEN=1");
        assert_eq!(got, want, "review JSON bytes changed for a host without agentsview");
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
                    "input_tokens": 5000,
                    "output_tokens": 250,
                    "cost_usd": null,
                }
            })
        );
        assert_eq!(parsed["schema_version"], 2);
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
