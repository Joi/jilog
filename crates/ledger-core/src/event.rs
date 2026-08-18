//! Event types -- the atoms of the ledger.
//!
//! # Rust concepts you'll learn here
//! - Structs with named fields
//! - Enums (Rust's algebraic data types)
//! - Derive macros: Debug, Clone, Serialize, Deserialize
//! - The `Option<T>` type for nullable fields
//! - String vs &str (owned vs borrowed)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The core event taxonomy. Keep this small.
/// Subsystem-specific details live in typed payloads, not here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventClass {
    Ingest,
    Route,
    Decision,
    StateChange,
    Claim,
    Delivery,
    Projection,
    Health,
    Approval,
    NoteMeta,
}

/// Payload confidentiality tiers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadTier {
    /// Only event metadata, no payload content.
    MetadataOnly,
    /// Structured payload, safe for local projection.
    Structured,
    /// Confidential detail, zone-local only.
    Confidential,
}

/// A single event in the ledger.
///
/// Events are immutable once written. They form an append-only log
/// within a segment file.
///
/// `PartialEq` supports deep content comparison of segments (spool
/// duplicate/conflict detection) — checksums alone are only 32 bits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Globally unique event identifier (UUIDv7 for time-ordering).
    pub event_id: Uuid,

    /// Which trust zone this event belongs to.
    pub zone: String,

    /// Source system that produced this event.
    pub source: String,

    /// Monotonic sequence number from the source.
    pub source_seq: u64,

    /// When the event occurred.
    pub timestamp: DateTime<Utc>,

    /// Correlation ID for grouping related events across a workflow.
    pub correlation_id: Option<Uuid>,

    /// ID of the event that directly caused this one.
    pub causation_id: Option<Uuid>,

    /// Who or what performed the action (person, system, agent).
    pub actor_ref: Option<String>,

    /// The object this event is about (claim, document, person, etc.).
    pub object_ref: Option<String>,

    /// What kind of event this is.
    pub event_class: EventClass,

    /// How much detail the payload contains.
    pub payload_tier: PayloadTier,

    /// The event payload. Structure depends on event_class.
    /// Using Value allows flexible typed payloads without an
    /// ever-expanding enum.
    pub payload: Option<serde_json::Value>,
}
