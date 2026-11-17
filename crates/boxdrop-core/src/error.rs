use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid hash: {0}")]
    InvalidHash(String),
    #[error("invalid identifier: {0}")]
    InvalidId(String),
    #[error("content hash mismatch: expected {expected}, got {actual}")]
    ContentMismatch { expected: String, actual: String },
    #[error("missing chunk {0}")]
    MissingChunk(String),
    #[error("path escapes the root: {0}")]
    UnsafePath(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, Error>;
