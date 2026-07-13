//! Signal enum and per-variant structs — the output types of the detector pipeline.

use serde::{Deserialize, Serialize};

/// A learning signal extracted from session transcripts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    Correction(Correction),
    Error(ErrorSignal),
    Workaround(Workaround),
    /// A mechanical session-health pattern (see [`crate::health`]).
    Pattern(PatternSignal),
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Correction {
    pub session_id: String,
    /// The short user message following an assistant turn.
    pub context: String,
    /// Which bot ran the session (fleet sessions only; see
    /// [`crate::reader::TranscriptHandle::persona`]). Stamped by
    /// [`crate::digest::run_review`], absent for coding sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Which group/surface the session serves (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// A `role: tool` message with `success: false` in its JSON content.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorSignal {
    pub session_id: String,
    pub tool_name: String,
    pub message: String,
    /// Which bot ran the session (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Which group/surface the session serves (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// Assistant text matching a workaround language pattern.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workaround {
    pub session_id: String,
    /// Human-readable pattern label (e.g. "for now", "TODO", "hack").
    pub pattern: String,
    /// First 200 chars of the matching assistant text.
    pub context: String,
    /// Which bot ran the session (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Which group/surface the session serves (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// A detected session-health pattern (compaction storm, stuck loop, resume
/// storm, iteration runaway — see [`crate::health`] for the detectors and
/// thresholds).
///
/// `pattern_kind` and `evidence` were added after the struct first shipped;
/// they default to empty when deserializing older serialized signals.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatternSignal {
    pub session_id: String,
    /// Human-readable one-line summary (used in issue titles).
    pub description: String,
    /// Stable snake_case detector id, e.g. "compaction_storm".
    #[serde(default)]
    pub pattern_kind: String,
    /// Compact factual backing, e.g. "4 compactions 09:01-09:08".
    #[serde(default)]
    pub evidence: String,
    /// Which bot ran the session (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Which group/surface the session serves (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// Assistant text postponing work to a later session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferralSignal {
    pub session_id: String,
    pub item: String,
    /// Which bot ran the session (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Which group/surface the session serves (fleet sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}
