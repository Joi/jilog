//! Error types for the SQLite projection layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqliteError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("ledger error: {0}")]
    Ledger(#[from] ledger_core::LedgerError),
}
