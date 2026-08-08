//! Plain-text (`.txt`) chunking with structure inference.

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
use common::ChunkRecordInput;

fn ensure_txt(file_path: &str) -> Result<()> {
    if file_path.to_ascii_lowercase().ends_with(".txt") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .txt file path, got: {file_path}"
        )))
    }
}

fn records_to_chunks(records: Vec<ChunkRecordInput>) -> Vec<Chunk> {
    records
        .into_iter()
        .map(|r| Chunk::new(r.content, r.content_type.as_str(), r.metadata))
        .collect()
}

fn build_records(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<ChunkRecordInput>> {
    let normalized = if mode == "default" { "structural" } else { mode };
    crate::options::validate_mode_args(
        normalized,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    let res = match normalized {
        "structural" => structural::build_chunks_from_txt_bytes(bytes),
        "section" => section::build_section_chunks(bytes),
        "semantic" => semantic::build_semantic_chunks(bytes),
        "sentence" => sentence::build_sentence_chunks(bytes, sentences_per_chunk),
        "page_aware" => page_aware::build_page_aware_chunks(bytes, paragraphs_per_page),
        "sliding_window" => sliding_window::build_sliding_window_chunks(bytes, window_size, overlap),
        other => return Err(ChunkError::InvalidArg(format!("Unknown TXT mode: {other}"))),
    };
    res.map_err(ChunkError::Parse)
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    ensure_txt(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    chunk_from_bytes(&bytes, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

/// No-filesystem entry (wasm/browser + bytes callers).
pub fn chunk_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    Ok(records_to_chunks(build_records(
        bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?))
}

/// No-filesystem Markdown (`.txt` is returned as-is, lossy-UTF8).
pub fn to_markdown_from_bytes(bytes: &[u8]) -> Result<String> {
    Ok(crate::text_encoding::decode_text(bytes).0)
}

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
                "TXT does not support mode '{}'",
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

pub fn to_markdown(file_path: &str) -> Result<String> {
    ensure_txt(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    Ok(crate::text_encoding::decode_text(&bytes).0)
}

pub fn stream(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<impl Iterator<Item = Result<Chunk>>> {
    Ok(chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?
        .into_iter()
        .map(Ok))
}
