//! Shared "assemble Markdown → reuse the md chunker" pipeline.
//!
//! Every prose-like format (json, eml, odf, msg, ipynb, rtf, pdf) parses its
//! source into `{markdown, images, document_metadata}` and then reuses the md
//! builders. This module is that shared tail so each format facade only has to
//! implement its own `load`.

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::md;
use crate::options::ChunkMode;

/// Map a unified [`ChunkMode`] onto the markdown-pipeline mode strings.
pub(crate) fn mode_str(mode: ChunkMode) -> Result<&'static str> {
    Ok(match mode {
        ChunkMode::Default => "default",
        ChunkMode::Structural => "structural",
        ChunkMode::Section => "section",
        ChunkMode::Semantic => "semantic",
        ChunkMode::Sentence => "sentence",
        ChunkMode::PageAware => "page_aware",
        ChunkMode::SlidingWindow => "sliding_window",
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "mode '{}' is not supported by markdown-pipeline formats",
                other.as_str()
            )))
        }
    })
}

/// A parsed prose document ready for the markdown chunker.
pub(crate) struct Loaded {
    pub markdown: String,
    pub images: Vec<(String, Vec<u8>)>,
    /// Document-level metadata injected as `document_metadata` on every chunk.
    pub metadata: serde_json::Value,
}

/// Deduplicate images by name, first occurrence winning — matching the Python
/// engine, which returns images as a `dict` (later duplicate keys collapse).
pub(crate) fn dedup_images(images: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::with_capacity(images.len());
    for (name, bytes) in images {
        if !out.iter().any(|(n, _)| n == &name) {
            out.push((name, bytes));
        }
    }
    out
}

/// The chunking modes every markdown-pipeline format supports.
pub(crate) const MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

fn build_records(
    loaded: &Loaded,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<md::common::ChunkRecordInput>> {
    // An empty document (adversarial/empty MIME, image-only notebook, …) yields
    // no markdown → zero chunks rather than an error, matching the Python engine.
    if loaded.markdown.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut records = md::build_records_from_bytes(
        loaded.markdown.as_bytes(),
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    for rec in records.iter_mut() {
        if let serde_json::Value::Object(map) = &mut rec.metadata {
            map.insert("document_metadata".to_string(), loaded.metadata.clone());
        }
    }
    Ok(records)
}

pub(crate) fn chunk(
    loaded: &Loaded,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    Ok(md::records_to_chunks(build_records(
        loaded,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?))
}

/// Dispatch-layer convenience: chunk a loaded doc from a unified [`ChunkOptions`].
pub(crate) fn chunk_opts(
    loaded: &Loaded,
    opts: &crate::options::ChunkOptions,
) -> Result<Vec<Chunk>> {
    let mode = mode_str(opts.mode)?;
    chunk(
        loaded,
        mode,
        opts.window_size,
        opts.overlap,
        opts.sentences_per_chunk,
        opts.paragraphs_per_page,
    )
}

/// Image chunks first (one per image, `content_type = "image"`), then text
/// chunks — matching the Python `_with_images` contract.
pub(crate) fn chunk_with_images(
    loaded: &Loaded,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let mut chunks: Vec<Chunk> = loaded
        .images
        .iter()
        .map(|(name, _)| Chunk::new(name.clone(), "image", serde_json::json!({ "image_name": name })))
        .collect();
    // A scanned PDF has images but no markdown to chunk. Running the text
    // chunker on an empty string errors ("Markdown file is empty after
    // decoding") and would throw the images away — the images ARE the output.
    if !loaded.markdown.trim().is_empty() {
        chunks.extend(chunk(
            loaded,
            mode,
            window_size,
            overlap,
            sentences_per_chunk,
            paragraphs_per_page,
        )?);
    }
    Ok((chunks, dedup_images(loaded.images.clone())))
}
