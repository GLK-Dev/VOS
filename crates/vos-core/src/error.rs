use thiserror::Error;

/// Domain-level errors shared across VOS crates.
#[derive(Debug, Error)]
pub enum VosError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid path '{0}': cannot represent as UTF-8")]
    InvalidPath(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),
}
