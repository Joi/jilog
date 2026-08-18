//! jilog CLI — thin wiring layer between TOML config and jilog-review.

mod config;
mod commands {
    pub mod review;
    pub mod query;
    pub mod spool;
}

use clap::{Parser, Subcommand};

use commands::review::ReviewArgs;
use commands::query::QueryArgs;

#[derive(Parser, Debug)]
#[command(
    name = "jilog",
    about = "Pluggable session-log review and append-only event ledger",
    version
)]
struct Cli {
    /// Path to jilog.toml (default: $JILOG_CONFIG, ~/.jilog.toml, ./jilog.toml).
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Review session transcripts for learning signals.
    Review(ReviewArgs),
    /// Query the append-only event ledger.
    Query(QueryArgs),
    /// Replicate ledger segments across the fleet (emit/ingest/status).
    Spool(commands::spool::SpoolArgs),
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    let cfg_path = cli.config.or_else(config::JilogConfig::default_path);
    let cfg = match cfg_path {
        Some(p) => config::JilogConfig::load(&p)?,
        None => {
            // Allow running with an empty config for --help and basic commands.
            config::JilogConfig::default()
        }
    };

    match cli.cmd {
        Cmd::Review(args) => commands::review::run(&cfg, args),
        Cmd::Query(args) => commands::query::run(&cfg, &args),
        Cmd::Spool(args) => match args.cmd {
            commands::spool::SpoolCmd::Emit(a) => commands::spool::run_emit(&cfg, a),
            commands::spool::SpoolCmd::Ingest(a) => commands::spool::run_ingest(&cfg, a),
            commands::spool::SpoolCmd::Status(a) => commands::spool::run_status(&cfg, a),
        },
    }
}
