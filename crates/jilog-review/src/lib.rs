//! jilog-review — pluggable session-log review pipeline.
//!
//! # Public surface
//!
//! - [`Signal`] enum + variants ([`Correction`], [`ErrorSignal`], [`Workaround`],
//!   [`PatternSignal`], [`DeferralSignal`])
//! - [`Reader`] trait + [`Message`], [`TranscriptHandle`], [`SessionEvent`],
//!   [`SessionStats`], [`ProcessedSessions`]
//! - [`Tracker`] trait + [`IssueRef`], [`signal_title`]
//! - Detectors: [`detect_corrections`], [`detect_errors`], [`detect_workarounds`],
//!   [`detect_deferrals`], [`detect_p0_alerts`]
//! - Health-pattern detectors over event streams: [`detect_health_patterns`]
//! - [`run_review`] — top-level orchestrator
//! - Built-in readers via [`readers`] module
//! - Built-in trackers via [`trackers`] module

pub mod error;
pub mod signal;
pub mod reader;
pub mod tracker;
pub mod detectors;
pub mod health;
pub mod digest;
pub mod util;
pub mod readers;
pub mod trackers;

pub use error::JilogReviewError;
pub use signal::{Signal, Correction, ErrorSignal, Workaround, PatternSignal, DeferralSignal};
pub use reader::{Reader, Message, TranscriptHandle, SessionEvent, SessionEventKind, SessionStats, ProcessedSessions, parse_session_role, is_sub_agent_session, SUB_AGENT_PREFIX};
pub use tracker::{Tracker, IssueRef, signal_title};
pub use detectors::{detect_corrections, detect_corrections_chat, detect_errors, detect_workarounds, detect_deferrals, detect_p0_alerts};
pub use health::detect_health_patterns;
pub use digest::{run_review, render_digest, write_digest, ReviewArgs, DigestReport, PersonaCounts, SpendSummary};
