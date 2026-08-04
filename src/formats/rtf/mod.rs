//! `.rtf` Rich Text Format chunking (markdown pipeline).

pub mod encoding;
pub mod extract;
pub mod fonts;
pub mod lists;
pub mod meta;
pub mod scan;
pub mod styles;
pub mod writer;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use extract::{extract, to_markdown as rtf_to_markdown};

fn ensure_rtf(file_path: &str) -> Result<()> {
    if file_path.to_ascii_lowercase().ends_with(".rtf") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .rtf file path, got: {file_path}"
        )))
    }
}

fn load(file_path: &str) -> Result<Loaded> {
    ensure_rtf(file_path)?;
    let bytes = std::fs::read(file_path).map_err(ChunkError::Io)?;
    load_bytes(&bytes)
}

/// No-filesystem entry (wasm/browser).
pub fn chunk_from_bytes(data: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    pipeline::chunk(&load_bytes(data)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8]) -> Result<String> {
    Ok(load_bytes(data)?.markdown)
}

fn load_bytes(bytes: &[u8]) -> Result<Loaded> {
    let doc = extract(bytes);
    let markdown = rtf_to_markdown(&doc);
    let metadata = serde_json::json!({
        "source_type": "rtf",
        "title": doc.title,
        "author": doc.author,
    });
    Ok(Loaded { markdown, images: Vec::new(), metadata })
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    pipeline::chunk(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    pipeline::chunk_opts(&load(file_path)?, opts)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(load(file_path)?.markdown)
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
