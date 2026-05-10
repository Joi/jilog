//! Zone identifiers -- trust boundaries with independent ledgers.
//!
//! # Rust concepts you'll learn here
//! - Newtypes (wrapping a type for type safety)
//! - Display trait implementation
//! - FromStr trait for parsing

use std::fmt;
use serde::{Deserialize, Serialize};

/// A trust zone identifier.
///
/// Each zone has its own independent ledger. Zone names are
/// configured, not hardcoded -- this keeps the code public-safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ZoneId(String);

impl ZoneId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ZoneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
