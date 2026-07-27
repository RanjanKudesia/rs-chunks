//! The universal chunk type returned by every format.
//!
//! Mirrors the `{content, content_type, metadata}` schema the Python engine
//! exposed, but as a native Rust struct. `metadata` stays a `serde_json::Value`
//! — exactly how every format already stores it internally — so nothing is lost
//! and downstream consumers can `serde` it into their own shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    pub content_type: String,
    pub metadata: Value,
}

impl Chunk {
    pub fn new(
        content: impl Into<String>,
        content_type: impl Into<String>,
        metadata: Value,
    ) -> Self {
        Self {
            content: content.into(),
            content_type: content_type.into(),
            metadata,
        }
    }
}
