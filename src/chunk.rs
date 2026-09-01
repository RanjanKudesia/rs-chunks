//! The universal chunk type returned by every format.
//!
//! Mirrors the `{content, content_type, metadata}` schema the Python engine
//! exposed, but as a native Rust struct. `metadata` stays a `serde_json::Value`
//! — exactly how every format already stores it internally — so nothing is lost
//! and downstream consumers can `serde` it into their own shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One chunk of a document — the universal product type every format returns.
///
/// Serialises to the same `{content, content_type, metadata}` JSON shape as the
/// `py-chunks` and `js-chunks` bindings (they are bindings over this struct).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// The chunk text. For structured formats this is rendered Markdown (tables,
    /// headings, list markers preserved); for plain formats it is the raw text
    /// span.
    pub content: String,
    /// What kind of unit this chunk is — e.g. `"section"`, `"semantic_block"`,
    /// `"sentence_group"`, `"sliding_window"`, `"page"`, `"row"`, `"table"`,
    /// `"sheet"`, `"record"`. The exact vocabulary is format- and mode-specific;
    /// see the metadata reference on chunkengine.dev.
    pub content_type: String,
    /// Format-specific provenance as a JSON object (source path/kind, indices,
    /// page/sheet/slide numbers, heading trail, merge diagnostics, …). Always a
    /// JSON object, never `null`; keys are format- and mode-specific.
    pub metadata: Value,
}

impl Chunk {
    /// Build a chunk from its three parts. `metadata` should be a JSON object
    /// (`serde_json::json!({...})`); every engine-produced chunk's is.
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

/// One image extracted from a document: `(file name, raw bytes)`.
///
/// The name is the engine-assigned one (see `image_naming`) and is what appears
/// in the owning chunk's `image_name` metadata, so callers can join the two.
pub type ExtractedImage = (String, Vec<u8>);

/// Every image extracted from one document, in document order.
pub type ExtractedImages = Vec<ExtractedImage>;

/// What the `*_with_images` chunkers return: the chunks, plus the images
/// extracted alongside them.
pub type ChunksWithImages = (Vec<Chunk>, ExtractedImages);

/// What the `to_markdown_with_images` renderers return: the rendered Markdown,
/// plus the images referenced from it.
pub type MarkdownWithImages = (String, ExtractedImages);
