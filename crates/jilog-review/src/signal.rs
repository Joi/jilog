//! Signal enum and per-variant structs — the output types of the detector pipeline.

use serde::{Deserialize, Serialize};

/// A learning signal extracted from session transcripts.
///
/// `Pattern` and `Deferral` are reserved for future detectors; no current
/// detector produces them. They appear in the enum for forward-compat only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    Correction(Correction),
    Error(ErrorSignal),
    Workaround(Workaround),
    /// Reserved: pattern detector not yet implemented.
    Pattern(PatternSignal),
    /// Reserved: deferral detector not yet implemented.
    Deferral(DeferralSignal),
}

impl Signal {
    /// Returns the session_id from whichever variant is active.
    pub fn session_id(&self) -> &str {
        match self {
            Signal::Correction(c) => &c.session_id,
            Signal::Error(e) => &e.session_id,
            Signal::Workaround(w) => &w.session_id,
            Signal::Pattern(p) => &p.session_id,
            Signal::Deferral(d) => &d.session_id,
        }
    }

    /// Returns a stable lowercase kind string matching the serde tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Signal::Correction(_) => "correction",
            Signal::Error(_) => "error",
            Signal::Workaround(_) => "workaround",
            Signal::Pattern(_) => "pattern",
            Signal::Deferral(_) => "deferral",
        }
    }
}

/// An assistant→user→assistant triple where the user message is short
/// (15..=200 chars), suggesting a correction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Correction {
    pub session_id: String,
    /// The short user message following an assistant turn.
    pub context: String,
}

/// A `role: tool` message with `success: false` in its JSON content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorSignal {
    pub session_id: String,
    pub tool_name: String,
    pub message: String,
}

/// Assistant text matching a workaround language pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workaround {
    pub session_id: String,
    /// Human-readable pattern label (e.g. "for now", "TODO", "hack").
    pub pattern: String,
    /// First 200 chars of the matching assistant text.
    pub context: String,
}

/// Reserved: detected recurring pattern. No detector produces this yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatternSignal {
    pub session_id: String,
    pub description: String,
}

/// Reserved: detected deferral. No detector produces this yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferralSignal {
    pub session_id: String,
    pub item: String,
}
