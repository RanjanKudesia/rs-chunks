//! Legacy PowerPoint binary (`.ppt`, OLE/CFB) chunking. Reuses the `.doc`
//! paragraph chunkers and metadata on paragraphs extracted from the PowerPoint
//! Document stream.

pub mod cfb_reader;
pub mod images;
pub mod records;
pub mod structural;
pub mod text_extractor;
pub mod to_markdown;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::doc::{records_to_chunks, validate_mode_args};
use crate::formats::doc::structural::{
    build_page_aware_chunks, build_section_chunks, build_semantic_chunks, build_sentence_chunks,
    build_sliding_window_chunks, build_structural_chunks,
};
use crate::options::{ChunkMode, ChunkOptions};
use structural::{load_ppt_paragraphs, validate_ppt_path};

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    validate_ppt_path(file_path).map_err(ChunkError::InvalidArg)?;
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let paragraphs = load_ppt_paragraphs(file_path).map_err(ChunkError::Parse)?;
    Ok(records_to_chunks(file_path, build_by_mode(paragraphs, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?))
}

fn build_by_mode(
    paragraphs: Vec<crate::formats::doc::text_extractor::DocParagraph>,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<crate::formats::doc::structural::ChunkRecord>> {
    Ok(match mode {
        "default" | "structural" => build_structural_chunks(paragraphs),
        "section" => build_section_chunks(paragraphs),
        "semantic" => build_semantic_chunks(paragraphs),
        "sentence" => build_sentence_chunks(paragraphs, sentences_per_chunk),
        "page_aware" => build_page_aware_chunks(paragraphs, paragraphs_per_page),
        "sliding_window" => build_sliding_window_chunks(paragraphs, window_size, overlap),
        other => return Err(ChunkError::InvalidArg(format!("Unknown PPT mode: {other}"))),
    })
}

/// No-filesystem entry (wasm/browser).
pub fn chunk_from_bytes(data: &[u8], source: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let paragraphs = structural::load_ppt_paragraphs_bytes(data).map_err(ChunkError::Parse)?;
    Ok(records_to_chunks(source, build_by_mode(paragraphs, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?))
}

pub fn to_markdown_from_bytes(data: &[u8]) -> Result<String> {
    let paragraphs = structural::load_ppt_paragraphs_bytes(data).map_err(ChunkError::Parse)?;
    Ok(crate::formats::doc::to_markdown::render_paragraphs_markdown(paragraphs))
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
                "PPT does not support mode '{}'",
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

/// Chunk with embedded images (ODRAW/blip extraction): image chunks first, then
/// text chunks, with indices renumbered across the combined list.
pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    validate_ppt_path(file_path).map_err(ChunkError::InvalidArg)?;
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let res = match mode {
        "default" | "structural" => images::chunk_with_images_impl(file_path, build_structural_chunks),
        "section" => images::chunk_with_images_impl(file_path, build_section_chunks),
        "semantic" => images::chunk_with_images_impl(file_path, build_semantic_chunks),
        "sentence" => images::chunk_with_images_impl(file_path, |p| build_sentence_chunks(p, sentences_per_chunk)),
        "page_aware" => images::chunk_with_images_impl(file_path, |p| build_page_aware_chunks(p, paragraphs_per_page)),
        "sliding_window" => images::chunk_with_images_impl(file_path, |p| build_sliding_window_chunks(p, window_size, overlap)),
        other => return Err(ChunkError::InvalidArg(format!("Unknown PPT mode: {other}"))),
    };
    res.map_err(ChunkError::Parse)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    validate_ppt_path(file_path).map_err(ChunkError::InvalidArg)?;
    let paragraphs = load_ppt_paragraphs(file_path).map_err(ChunkError::Parse)?;
    Ok(crate::formats::doc::to_markdown::render_paragraphs_markdown(paragraphs))
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    to_markdown::to_markdown_with_images(file_path).map_err(ChunkError::Parse)
}

/// No-filesystem `chunk_with_images` (wasm/browser). `filename` is recorded as
/// each chunk's `source`.
pub fn chunk_with_images_from_bytes(bytes: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let res = match mode {
        "default" | "structural" => images::chunk_with_images_impl_bytes(bytes, filename, build_structural_chunks),
        "section" => images::chunk_with_images_impl_bytes(bytes, filename, build_section_chunks),
        "semantic" => images::chunk_with_images_impl_bytes(bytes, filename, build_semantic_chunks),
        "sentence" => images::chunk_with_images_impl_bytes(bytes, filename, |p| build_sentence_chunks(p, sentences_per_chunk)),
        "page_aware" => images::chunk_with_images_impl_bytes(bytes, filename, |p| build_page_aware_chunks(p, paragraphs_per_page)),
        "sliding_window" => images::chunk_with_images_impl_bytes(bytes, filename, |p| build_sliding_window_chunks(p, window_size, overlap)),
        other => return Err(ChunkError::InvalidArg(format!("Unknown PPT mode: {other}"))),
    };
    res.map_err(ChunkError::Parse)
}

pub fn to_markdown_with_images_from_bytes(bytes: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    to_markdown::to_markdown_with_images_bytes(bytes).map_err(ChunkError::Parse)
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
