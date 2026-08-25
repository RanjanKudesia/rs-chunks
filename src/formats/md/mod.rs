//! Markdown chunking — the foundational prose engine.
//!
//! Seven prose-like formats (json, eml, odf, msg, ipynb, rtf, pdf) assemble
//! clean Markdown and reuse these builders through
//! `crate::formats::pipeline`, so `md` is ported first. The `build_*`
//! functions are shared crate-internally via `build_records_from_bytes`.
//!
//! `epub` is **not** one of them despite what this comment used to say — it
//! reuses `html`'s builder family, as `txt` does. That is eight formats on this
//! module, not ten ([#87](TECH_DEBT.md)).

pub(crate) mod block_stream;
pub mod common;
pub(crate) mod page_aware;
pub(crate) mod section;
pub(crate) mod semantic;
pub(crate) mod sentence;
pub(crate) mod sliding_window;
pub(crate) mod structural;

use std::fs;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use common::{ChunkRecordInput, SpannedRecord};

/// Stream a markdown file's chunks. Markdown parses in one pass, so the whole
/// document is chunked up front and drained lazily — the profile
/// `streaming.mdx` records for it.
pub fn stream(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<impl Iterator<Item = Result<Chunk>>> {
    Ok(chunk(
        file_path,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?
    .into_iter()
    .map(Ok))
}

/// Map the internal record type onto the public [`Chunk`].
pub(crate) fn records_to_chunks(records: Vec<ChunkRecordInput>) -> Vec<Chunk> {
    records
        .into_iter()
        .map(|r| Chunk::new(r.content, r.content_type.as_str(), r.metadata))
        .collect()
}

/// Build records for a given mode from raw markdown bytes. Reused by every
/// markdown-pipeline format.
pub(crate) fn build_records_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<SpannedRecord>> {
    crate::options::validate_mode_args(
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    let res = match mode {
        "default" | "structural" => structural::build_chunks_from_md_bytes(bytes),
        "section" => section::build_section_chunks(bytes),
        "semantic" => semantic::build_semantic_chunks(bytes),
        "sentence" => sentence::build_sentence_chunks(bytes, sentences_per_chunk),
        "page_aware" => page_aware::build_page_aware_chunks(bytes, paragraphs_per_page),
        "sliding_window" => {
            sliding_window::build_sliding_window_chunks(bytes, window_size, overlap)
        }
        other => return Err(ChunkError::InvalidArg(format!("Unknown MD mode: {other}"))),
    };
    res.map_err(ChunkError::Parse)
}

const VALID_MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

/// Chunk a `.md` file. `mode` ∈ {default, structural, section, semantic,
/// sentence, page_aware, sliding_window}.
pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    if !file_path.to_ascii_lowercase().ends_with(".md") {
        return Err(ChunkError::InvalidArg(format!(
            "Expected .md file path, got: {file_path}"
        )));
    }
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    chunk_from_bytes(
        &bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

/// No-filesystem entry: chunk Markdown from raw bytes (used by the wasm/browser
/// build and by callers holding bytes instead of a path).
pub fn chunk_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    if !VALID_MODES.contains(&mode) {
        return Err(ChunkError::InvalidArg(format!(
            "mode must be one of {VALID_MODES:?} for MD, got: '{mode}'"
        )));
    }
    let records = build_records_from_bytes(
        bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    // A `.md` file has no records to point at, so the spans are dropped here.
    Ok(records_to_chunks(
        records.into_iter().map(|s| s.record).collect(),
    ))
}

/// No-filesystem Markdown passthrough (a `.md` document is already Markdown).
pub fn to_markdown_from_bytes(bytes: &[u8]) -> Result<String> {
    // Decoded, not validated. `get_markdown` and `get_chunks` must decode a
    // document identically (TECH_DEBT #75, #89, C7) and they did not: this
    // hard-errored with "MD not valid UTF-8" on cp1252 bytes while the six
    // chunk strategies decoded them lossily into U+FFFD. One document, two
    // answers, neither of them the text.
    //
    // Markdown has no in-band encoding declaration, so unlike HTML there is
    // nothing extra to consult — it is exactly the `.txt` ladder: BOM, UTF-16
    // sniff, valid UTF-8, then cp1252. Never a hard error, never U+FFFD.
    Ok(common::decode_body(bytes))
}

/// Dispatch-layer entry: map a unified [`ChunkOptions`] onto MD's strategies.
pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    let mode = match opts.mode {
        ChunkMode::Default => "default",
        ChunkMode::Structural => "structural",
        ChunkMode::Section => "section",
        ChunkMode::Semantic => "semantic",
        ChunkMode::Sentence => "sentence",
        ChunkMode::PageAware => "page_aware",
        ChunkMode::SlidingWindow => "sliding_window",
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "MD does not support mode '{}'",
                other.as_str()
            )))
        }
    };
    chunk(
        file_path,
        mode,
        opts.window_size,
        opts.overlap,
        opts.sentences_per_chunk,
        opts.paragraphs_per_page,
    )
}

/// A `.md` file is already Markdown; `to_markdown` returns its text.
pub fn to_markdown(file_path: &str) -> Result<String> {
    if !file_path.to_ascii_lowercase().ends_with(".md") {
        return Err(ChunkError::InvalidArg(format!(
            "Expected .md file path, got: {file_path}"
        )));
    }
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    // Delegate rather than repeat the decode, so the path and bytes entry
    // points cannot drift apart again — which is how they came to disagree.
    to_markdown_from_bytes(&bytes)
}
