//! Error types for the ledger.
//!
//! # Rust concepts you'll learn here
//! - thiserror for ergonomic error enums
//! - The Error trait
//! - Enum variants with data

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("integrity check failed: {0}")]
    IntegrityFailure(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // Note: field renamed `src` to avoid thiserror v2 treating `source` as
    // the error source (which requires the field to impl std::error::Error).
    #[error("segment {src}:{seq} already exists")]
    DuplicateSegment { src: String, seq: u64 },

    #[error("missing segment: expected {src}:{expected}, got {src}:{got}")]
    MissingSegment {
        src: String,
        expected: u64,
        got: u64,
    },
}
