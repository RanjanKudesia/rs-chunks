//! PowerPoint OOXML (`.pptx` / `.potx` / `.potm` / `.ppsx` / `.ppsm`) chunking.
//! Every mode builds directly from the package bytes.

pub mod common;
mod page_aware;
mod section;
mod semantic;
mod sentence;
mod sliding_window;
mod structural;
mod to_markdown;

use std::fs;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use common::ChunkRecordInput;

fn ensure_pptx(file_path: &str) -> Result<()> {
    if common::is_pptx_ooxml(file_path) {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected a PowerPoint OOXML file ({}), got: {file_path}",
            common::pptx_exts_display()
        )))
    }
}

fn to_chunks(records: Vec<ChunkRecordInput>) -> Vec<Chunk> {
    records
        .into_iter()
        .map(|c| Chunk::new(c.content, c.content_type.as_str(), c.metadata))
        .collect()
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    ensure_pptx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    chunk_from_bytes(&bytes, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

/// No-filesystem entry (wasm/browser).
pub fn chunk_from_bytes(bytes: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    let res = match mode {
        "default" | "structural" => structural::build_structural_chunks(bytes),
        "section" => section::build_section_chunks(bytes),
        "semantic" => semantic::build_semantic_chunks(bytes),
        "sentence" => {
            if sentences_per_chunk == 0 {
                return Err(ChunkError::InvalidArg("sentences_per_chunk must be greater than 0".into()));
            }
            sentence::build_sentence_chunks(bytes, sentences_per_chunk)
        }
        "page_aware" => {
            if paragraphs_per_page == 0 {
                return Err(ChunkError::InvalidArg("paragraphs_per_page must be greater than 0".into()));
            }
            page_aware::build_page_aware_chunks(bytes, paragraphs_per_page)
        }
        "sliding_window" => {
            if window_size == 0 {
                return Err(ChunkError::InvalidArg("window_size must be greater than 0".into()));
            }
            if overlap >= window_size {
                return Err(ChunkError::InvalidArg("overlap must be less than window_size".into()));
            }
            sliding_window::build_sliding_window_chunks(bytes, window_size, overlap)
        }
        other => return Err(ChunkError::InvalidArg(format!("Unknown PPTX mode: {other}"))),
    };
    res.map(to_chunks).map_err(ChunkError::Parse)
}

/// No-filesystem Markdown (wasm/browser).
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
                "PPTX does not support mode '{}'",
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

/// Chunk with embedded slide images: image chunks (with slide provenance) first,
/// then text chunks, plus the extracted image bytes.
pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    ensure_pptx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    chunk_with_images_from_bytes(&bytes, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(bytes: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let mut archive = common::open_pptx(bytes).map_err(ChunkError::Parse)?;
    let slide_names = common::collect_slide_names(&archive);
    let total_slides = slide_names.len();
    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    let image_infos = common::collect_all_slide_images(&mut archive, &slide_names, total_slides, &mut image_out);

    let mut chunks: Vec<Chunk> = image_infos
        .into_iter()
        .map(|info| {
            Chunk::new(
                info.hash_name.clone(),
                "image",
                serde_json::json!({
                    "slide_number": info.slide_num,
                    "image_name": info.hash_name,
                    "alt_text": info.alt_text,
                    "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
                }),
            )
        })
        .collect();
    chunks.extend(chunk_from_bytes(bytes, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?);
    Ok((chunks, image_out))
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    ensure_pptx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    to_markdown::to_markdown(&bytes).map_err(ChunkError::Parse)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    ensure_pptx(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    to_markdown_with_images_from_bytes(&bytes)
}

pub fn to_markdown_with_images_from_bytes(bytes: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>)> {
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
    Ok(chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?
        .into_iter()
        .map(Ok))
}
