//! Error type for the chunking engine.
//!
//! The internal format logic already returns `Result<_, String>` everywhere;
//! at the public boundary those strings map onto structured variants so callers
//! can distinguish "you asked for something unsupported" from "the file failed
//! to parse". Adversarial fixtures must land here as a clean `Err` — never a
//! panic.

use std::fmt;

/// Error type returned by every public entry point.
///
/// Marked `#[non_exhaustive]`: downstream `match`es need a wildcard arm, so new
/// variants can be added without a semver-major bump.
#[derive(Debug)]
#[non_exhaustive]
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

/// Stringify a `catch_unwind` payload.
///
/// Shared so every panic boundary in the engine produces the same message for
/// the same panic — the dispatch boundary and the PDF stream workers were
/// otherwise going to word it twice.
pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_string())
}

#[cfg(test)]
mod panic_message_tests {
    use super::panic_message;

    /// Both panic boundaries — `dispatch` and the two PDF stream workers — word
    /// a panic identically, so a caller cannot tell which thread it came from.
    /// Pinned because the wording is now shared rather than duplicated.
    #[test]
    fn stringifies_both_payload_shapes_and_neither() {
        let s = std::panic::catch_unwind(|| panic!("static str")).unwrap_err();
        assert_eq!(panic_message(s), "static str");

        let owned = std::panic::catch_unwind(|| panic!("{}", String::from("owned"))).unwrap_err();
        assert_eq!(panic_message(owned), "owned");

        let odd = std::panic::catch_unwind(|| std::panic::panic_any(42u8)).unwrap_err();
        assert_eq!(panic_message(odd), "unknown panic payload");
    }
}
