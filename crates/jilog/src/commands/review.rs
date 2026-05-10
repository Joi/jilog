//! `jilog review` — session transcript review pipeline.

use anyhow::Context;
use chrono::{Duration, NaiveDate, Utc};

use jilog_review::digest::ReviewArgs as LibReviewArgs;
use jilog_review::util::expand_tilde;

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
    let since = Utc::now() - Duration::days(args.days as i64);

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
    let tracker = cfg.into_tracker();

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

    println!(
        "{} corrections, {} errors, {} workarounds, {} P0 alert(s), {} session(s) scanned",
        report.corrections.len(),
        report.errors.len(),
        report.workarounds.len(),
        report.p0_alerts.len(),
        report.sessions_scanned,
    );

    if !args.dry_run {
        println!("Digest: {}", report.digest_path.display());
    }

    if !report.created_issues.is_empty() {
        println!("Created {} issue(s)", report.created_issues.len());
    }

    Ok(())
}
