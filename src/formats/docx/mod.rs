//! Word OOXML (`.docx` / `.docm` / `.dotx` / `.dotm`) chunking. Each mode parses
//! the package differently (indexed paragraphs, paragraph events, block tree),
//! then builds chunks — faithful to the reference engine.

mod block_api;
mod block_model;
pub mod common;
mod docx_aux;
mod harvest;
mod images_rels;
mod page_aware;
mod section;
mod semantic;
mod sentence;
mod sliding_window;
mod stream_walker;
mod structural;
mod structural_build;
mod structural_build_images;
mod structural_model;
mod structural_text;
mod table_render;
mod to_markdown;
mod xml_text;

use std::fs;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};

fn ensure_docx(file_path: &str) -> Result<()> {
    if common::is_word_ooxml(file_path) {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected a Word OOXML file ({}), got: {file_path}",
            common::word_exts_display()
        )))
    }
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    ensure_docx(file_path)?;
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

/// No-filesystem entry (wasm/browser + bytes callers).
pub fn chunk_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let res = match mode {
        "default" | "structural" => structural::chunk(bytes),
        "section" => section::chunk(bytes),
        "semantic" => semantic::chunk(bytes),
        "sentence" => {
            if sentences_per_chunk == 0 {
                return Err(ChunkError::InvalidArg(
                    "sentences_per_chunk must be greater than 0".into(),
                ));
            }
            sentence::chunk(bytes, sentences_per_chunk)
        }
        "page_aware" => {
            if paragraphs_per_page == 0 {
                return Err(ChunkError::InvalidArg(
                    "paragraphs_per_page must be greater than 0".into(),
                ));
            }
            page_aware::chunk(bytes, paragraphs_per_page)
        }
        "sliding_window" => {
            if window_size == 0 {
                return Err(ChunkError::InvalidArg(
                    "window_size must be greater than 0".into(),
                ));
            }
            if overlap >= window_size {
                return Err(ChunkError::InvalidArg(
                    "overlap must be less than window_size".into(),
                ));
            }
            sliding_window::chunk(bytes, window_size, overlap)
        }
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "Unknown DOCX mode: {other}"
            )))
        }
    };
    res.map_err(ChunkError::Parse)
}

/// No-filesystem Markdown.
pub fn to_markdown_from_bytes(bytes: &[u8]) -> Result<String> {
    to_markdown::to_markdown(bytes).map_err(ChunkError::Parse)
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
                "DOCX does not support mode '{}'",
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

/// Chunk with embedded images: image chunks (`content_type = "image"`) plus text
/// chunks, and the extracted image bytes. `mode` matches [`chunk`].
pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    ensure_docx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    chunk_with_images_from_bytes(
        &bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    let res = match mode {
        "default" | "structural" => structural::chunk_with_images(bytes),
        "section" => section::chunk_with_images(bytes),
        "semantic" => semantic::chunk_with_images(bytes),
        "sentence" => {
            if sentences_per_chunk == 0 {
                return Err(ChunkError::InvalidArg(
                    "sentences_per_chunk must be greater than 0".into(),
                ));
            }
            sentence::chunk_with_images(bytes, sentences_per_chunk)
        }
        "page_aware" => {
            if paragraphs_per_page == 0 {
                return Err(ChunkError::InvalidArg(
                    "paragraphs_per_page must be greater than 0".into(),
                ));
            }
            page_aware::chunk_with_images(bytes, paragraphs_per_page)
        }
        "sliding_window" => {
            if window_size == 0 {
                return Err(ChunkError::InvalidArg(
                    "window_size must be greater than 0".into(),
                ));
            }
            if overlap >= window_size {
                return Err(ChunkError::InvalidArg(
                    "overlap must be less than window_size".into(),
                ));
            }
            sliding_window::chunk_with_images(bytes, window_size, overlap)
        }
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "Unknown DOCX mode: {other}"
            )))
        }
    };
    res.map_err(ChunkError::Parse)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    ensure_docx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    to_markdown::to_markdown(&bytes).map_err(ChunkError::Parse)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<crate::chunk::MarkdownWithImages> {
    ensure_docx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    to_markdown_with_images_from_bytes(&bytes)
}

pub fn to_markdown_with_images_from_bytes(
    bytes: &[u8],
) -> Result<crate::chunk::MarkdownWithImages> {
    to_markdown::to_markdown_with_images(bytes).map_err(ChunkError::Parse)
}

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
