//! HTML / HTM chunking with its own block parser + Markdown conversion.

pub mod common;
pub(crate) mod encoding;
pub mod images;
pub(crate) mod page_aware;
pub(crate) mod section;
pub(crate) mod semantic;
pub(crate) mod sentence;
pub(crate) mod sliding_window;
pub(crate) mod structural;
pub mod to_markdown;

use std::fs;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use common::ChunkRecordInput;
use encoding::decode_html;
use images::collect_html_images;
use to_markdown::html_to_markdown_str;

fn ensure_html(file_path: &str) -> Result<()> {
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .html or .htm file path, got: {file_path}"
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
    let normalized = if mode == "default" {
        "structural"
    } else {
        mode
    };
    crate::options::validate_mode_args(
        normalized,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    let res = match normalized {
        "structural" => structural::build_chunks_from_html_bytes(bytes),
        "section" => section::build_section_chunks(bytes),
        "semantic" => semantic::build_semantic_chunks(bytes),
        "sentence" => sentence::build_sentence_chunks(bytes, sentences_per_chunk),
        "page_aware" => page_aware::build_page_aware_chunks(bytes, paragraphs_per_page),
        "sliding_window" => {
            sliding_window::build_sliding_window_chunks(bytes, window_size, overlap)
        }
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "Unknown HTML mode: {other}"
            )))
        }
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
    ensure_html(file_path)?;
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

/// No-filesystem entry (wasm/browser).
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

/// No-filesystem Markdown (wasm/browser).
pub fn to_markdown_from_bytes(bytes: &[u8]) -> Result<String> {
    Ok(html_to_markdown_str(&decode_html(bytes)))
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
                "HTML does not support mode '{}'",
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

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    ensure_html(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    let html = decode_html(&bytes);
    let text_records = build_records(
        &bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;

    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    let image_infos = collect_html_images(&html, file_path, &mut image_out);

    let mut chunks: Vec<Chunk> = image_infos
        .into_iter()
        .map(|info| {
            Chunk::new(
                info.hash_name.clone(),
                "image",
                serde_json::json!({
                    "image_name": info.hash_name,
                    "alt_text": info.alt_text,
                    "document_metadata": { "source_type": "html" }
                }),
            )
        })
        .collect();
    chunks.extend(records_to_chunks(text_records));
    Ok((chunks, image_out))
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    ensure_html(file_path)?;
    // Read bytes, not a String: `read_to_string` rejects any document that is
    // not UTF-8, which is how a windows-1251 page became an `io::Error` (C7).
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    Ok(html_to_markdown_str(&decode_html(&bytes)))
}

pub fn to_markdown_with_images(file_path: &str) -> Result<crate::chunk::MarkdownWithImages> {
    ensure_html(file_path)?;
    let bytes = fs::read(file_path).map_err(ChunkError::Io)?;
    let html = decode_html(&bytes);
    let mut markdown = html_to_markdown_str(&html);
    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    let image_infos = collect_html_images(&html, file_path, &mut image_out);
    for info in &image_infos {
        markdown.push_str(&format!("\n\n![]({})", info.hash_name));
    }
    Ok((markdown.trim_start().to_string(), image_out))
}

/// No-filesystem `chunk_with_images` (wasm/browser). Relative-path `<img src>`
/// cannot be resolved without a filesystem; embedded data-URI images still work.
pub fn chunk_with_images_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    let html = decode_html(bytes);
    let text_records = build_records(
        bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    let image_infos = collect_html_images(&html, "", &mut image_out);
    let mut chunks: Vec<Chunk> = image_infos.into_iter().map(|info| Chunk::new(info.hash_name.clone(), "image", serde_json::json!({"image_name": info.hash_name, "alt_text": info.alt_text, "document_metadata": {"source_type": "html"}}))).collect();
    chunks.extend(records_to_chunks(text_records));
    Ok((chunks, image_out))
}

pub fn to_markdown_with_images_from_bytes(
    bytes: &[u8],
) -> Result<crate::chunk::MarkdownWithImages> {
    let html = decode_html(bytes);
    let mut markdown = html_to_markdown_str(&html);
    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    let image_infos = collect_html_images(&html, "", &mut image_out);
    for info in &image_infos {
        markdown.push_str(&format!("\n\n![]({})", info.hash_name));
    }
    Ok((markdown.trim_start().to_string(), image_out))
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
