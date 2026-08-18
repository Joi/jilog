//! ledger-spool: Cross-machine append-only spool transport.
//!
//! Producers (every machine in the fleet) write their own event
//! segments to a spool directory. The spool replicates via Syncthing
//! to the configured authority host (in this fleet, jibotmac). The
//! authority ingests, validates, deduplicates, and commits segments
//! to the authoritative ledger store — it is the fleet store's only
//! writer (single-writer discipline).
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
//! - **No-clobber moves (`hard_link` + remove)**: processed segments
//!   move from `incoming/` to `processed/` via `std::fs::hard_link` —
//!   which FAILS if the destination exists, unlike rename — followed by
//!   removing the incoming copy. Nothing is deleted outright and
//!   nothing is ever silently overwritten; `processed/` is the audit
//!   trail. This is the same durability posture databases take with
//!   WAL files.
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

/// Validate a segment source name: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
///
/// Both ends of the spool enforce this — `jilog spool emit` refuses to
/// run with a non-conforming source, and the ingester rejects any
/// spooled segment whose `source` fails it. Since `Segment::filename()`
/// derives the on-disk path from the source, this is also the guard
/// against path traversal (`/`, `..`) and other filename spoofing.
pub fn valid_source_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::valid_source_name;

    #[test]
    fn source_name_pattern() {
        for good in ["jibotmac", "macazbd", "host-2", "a", "A.b_c-9", "0x"] {
            assert!(valid_source_name(good), "{good:?} should be valid");
        }
        for bad in [
            "",
            "../evil",
            "a/b",
            ".hidden",
            "-dash",
            "_under",
            "has space",
            "host\0",
            &"x".repeat(65),
        ] {
            assert!(!valid_source_name(bad), "{bad:?} should be invalid");
        }
        assert!(valid_source_name(&"x".repeat(64)), "64 chars is the max");
    }
}
