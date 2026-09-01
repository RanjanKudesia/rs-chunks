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
    pub images: crate::chunk::ExtractedImages,
    /// Document-level metadata injected as `document_metadata` on every chunk.
    pub metadata: serde_json::Value,
    /// For a format whose source is a sequence of records — `.json`, `.jsonl`,
    /// `.ndjson` — the markdown block each record starts at, in order. This is
    /// what turns a chunk's block span into the records it was built from
    /// ([#46](TECH_DEBT.md)). `None` for a format that has no records, and then
    /// no chunk gains a `record_range`.
    pub records: Option<Vec<usize>>,
}

/// The records a span of markdown blocks came from, as an inclusive 0-based
/// range. `starts[i]` is the first block of record `i`, so the record covering
/// a block is the last one that starts at or before it.
fn records_for(starts: &[usize], span: (usize, usize)) -> Option<(usize, usize)> {
    if starts.is_empty() {
        return None;
    }
    // The last record that starts at or before the block. `partition_point`
    // rather than `binary_search` because a record that renders to nothing
    // shares its successor's start, and the owner is the later one.
    let record_of = |block: usize| {
        starts
            .partition_point(|start| *start <= block)
            .saturating_sub(1)
    };
    Some((record_of(span.0), record_of(span.1)))
}

/// Deduplicate images by name, first occurrence winning — matching the Python
/// engine, which returns images as a `dict` (later duplicate keys collapse).
pub(crate) fn dedup_images(images: crate::chunk::ExtractedImages) -> crate::chunk::ExtractedImages {
    let mut out: crate::chunk::ExtractedImages = Vec::with_capacity(images.len());
    for (name, bytes) in images {
        if !out.iter().any(|(n, _)| n == &name) {
            out.push((name, bytes));
        }
    }
    out
}

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
    let records = md::build_records_from_bytes(
        loaded.markdown.as_bytes(),
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    let mut out = Vec::with_capacity(records.len());
    for spanned in records.into_iter() {
        out.push(stamp(spanned, &loaded.metadata, loaded.records.as_deref()));
    }
    Ok(out)
}

/// Attach the document-level metadata a finished record still needs.
///
/// Split out so the incremental PDF path ([`super::pdf::stream`]) stamps its
/// chunks with exactly this code rather than a copy of it — the two must agree
/// byte for byte, and `stream_matches_batch_for_every_mode` only catches a
/// divergence if there is one place to get it right.
pub(crate) fn stamp(
    spanned: md::common::SpannedRecord,
    metadata: &serde_json::Value,
    records: Option<&[usize]>,
) -> md::common::ChunkRecordInput {
    let mut rec = spanned.record;
    if let serde_json::Value::Object(map) = &mut rec.metadata {
        map.insert("document_metadata".to_string(), metadata.clone());
        // The block span is internal. It only becomes visible metadata for a
        // format that has records to name, which is why adding it changed
        // nothing for `.md`, `.html`, `.txt`, `.pdf` and the rest.
        if let (Some(starts), Some(span)) = (records, spanned.blocks) {
            if let Some((first, last)) = records_for(starts, span) {
                // Range only. A sibling `record_count` was in the design, but
                // `document_metadata.record_count` already means the *file's*
                // total, and two different counts under one name is a trap; the
                // chunk's own count is `last - first + 1`.
                map.insert("record_range".to_string(), serde_json::json!([first, last]));
            }
        }
    }
    rec
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
) -> Result<crate::chunk::ChunksWithImages> {
    let mut chunks: Vec<Chunk> = loaded
        .images
        .iter()
        .map(|(name, _)| {
            Chunk::new(
                name.clone(),
                "image",
                serde_json::json!({ "image_name": name }),
            )
        })
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
