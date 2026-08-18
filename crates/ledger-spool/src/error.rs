//! Error types for the spool transport.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ledger error: {0}")]
    Ledger(#[from] ledger_core::LedgerError),

    #[error("serialization error: {0}")]
    Serialization(String),

    // Note: field renamed `src` to avoid thiserror v2 treating `source`
    // as the error source chain (requires impl std::error::Error).
    #[error("integrity check failed for {src}:{seq}: {reason}")]
    IntegrityFailure {
        src: String,
        seq: u64,
        reason: String,
    },

    #[error(
        "invalid segment source {name:?}: must match ^[A-Za-z0-9][A-Za-z0-9._-]{{0,63}}$"
    )]
    InvalidSource { name: String },

    #[error(
        "spool filename {found:?} does not match segment identity {expected:?} \
         (path-traversal / spoof guard)"
    )]
    IdentityMismatch { found: String, expected: String },
}
