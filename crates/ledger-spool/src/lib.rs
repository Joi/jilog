//! ledger-spool: Cross-machine append-only spool transport.
//!
//! Producers (e.g., jibotmac) write event segments to a spool directory.
//! The spool replicates via Syncthing to the authority machine (macazbd).
//! The authority ingests, validates, deduplicates, and commits segments
//! to the authoritative ledger store.
//!
//! # Spool directory layout
//!
//! ```text
//! spool/{zone}/
//!   incoming/           # Segments land here (via Syncthing)
//!     macazbd-000001.json
//!     jibotmac-000001.json
//!   processed/          # After successful ingestion (audit trail)
//!     jibotmac-000001.json
//! ```
//!
//! # Rust concepts in this crate
//!
//! - **`std::fs::rename`**: Atomic move within the same filesystem.
//!   We move processed segments from `incoming/` to `processed/`
//!   rather than deleting them, creating an audit trail. This is the
//!   same pattern databases use for WAL files.
//!
//! - **Error accumulation**: The ingester processes ALL segments in the
//!   spool, even if some fail. Failures are collected into a Vec and
//!   returned alongside successes. This "best effort" pattern is common
//!   in batch processing -- you want to ingest what you can, not stop
//!   on the first bad segment.

pub mod writer;
pub mod ingester;
pub mod error;

pub use writer::SpoolWriter;
pub use ingester::{SpoolIngester, IngestReport};
pub use error::SpoolError;
