//! Error type for the chunking engine.
//!
//! The internal format logic already returns `Result<_, String>` everywhere;
//! at the public boundary those strings map onto structured variants so callers
//! can distinguish "you asked for something unsupported" from "the file failed
//! to parse". Adversarial fixtures must land here as a clean `Err` — never a
//! panic.

use std::fmt;

#[derive(Debug)]
pub enum ChunkError {
    /// The file extension / format is not handled by the engine.
    Unsupported(String),
    /// A caller-supplied argument was invalid (bad mode, window/overlap, …).
    InvalidArg(String),
    /// The document could not be parsed / decoded.
    Parse(String),
    /// Underlying I/O failure.
    Io(std::io::Error),
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChunkError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ChunkError::InvalidArg(m) => write!(f, "invalid argument: {m}"),
            ChunkError::Parse(m) => write!(f, "parse error: {m}"),
            ChunkError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for ChunkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ChunkError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ChunkError {
    fn from(e: std::io::Error) -> Self {
        ChunkError::Io(e)
    }
}

/// Convenience: the format helpers return `Result<_, String>`; use this to lift
/// those parse-side strings into `ChunkError::Parse`.
impl From<String> for ChunkError {
    fn from(m: String) -> Self {
        ChunkError::Parse(m)
    }
}

pub type Result<T> = std::result::Result<T, ChunkError>;
