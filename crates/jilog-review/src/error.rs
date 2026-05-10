//! JilogReviewError — unified error type for the jilog-review crate.

/// Unified error type for all jilog-review operations.
#[derive(Debug, thiserror::Error)]
pub enum JilogReviewError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("tracker backend: {0}")]
    Tracker(String),
    #[error("reader backend: {0}")]
    Reader(String),
    #[error("invalid config: {0}")]
    Config(String),
    #[error("external command failed: {0}")]
    Command(String),
}
