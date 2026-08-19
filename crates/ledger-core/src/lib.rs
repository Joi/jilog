//! ledger-core: Append-only event ledger with per-zone segment files.
//!
//! This crate defines the core types for the event ledger:
//! - Events (the atoms of the ledger)
//! - Segments (append-only files containing ordered events)
//! - Zones (trust boundaries with independent ledgers)
//!
//! # Architecture
//!
//! Segment files are the **authority**. SQLite (in ledger-sqlite) is a
//! rebuildable projection. Nothing here depends on a database.

pub mod event;
pub mod segment;
pub mod store;
pub mod zone;
pub mod error;
#[doc(hidden)]
pub mod test_support;

pub use event::{Event, EventClass, PayloadTier};
pub use segment::{PublishOutcome, Segment};
pub use store::SegmentStore;
pub use zone::ZoneId;
pub use error::LedgerError;
