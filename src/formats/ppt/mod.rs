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
use crate::formats::doc::validate_mode_args;
use crate::formats::doc::structural::ChunkRecord;
use crate::formats::doc::text_extractor::{DocParagraph, ParagraphType};
use crate::formats::doc::structural::{
    build_page_aware_chunks, build_section_chunks, build_semantic_chunks, build_sentence_chunks,
    build_sliding_window_chunks, build_structural_chunks,
};
use crate::options::{ChunkMode, ChunkOptions};
use structural::{load_ppt_paragraphs, validate_ppt_path};

/// Slide titles by 0-based ordinal: the first heading paragraph of each slide,
/// matching how `text_extractor` classifies a slide's title placeholder.
pub(super) fn slide_titles(stream_paragraphs: &[DocParagraph]) -> std::collections::HashMap<usize, String> {
    let mut titles = std::collections::HashMap::new();
    for p in stream_paragraphs {
        let Some(idx) = p.page_index else { continue };
        if matches!(p.paragraph_type, ParagraphType::Heading(_)) && !p.content.trim().is_empty() {
            titles.entry(idx).or_insert_with(|| p.content.trim().to_string());
        }
    }
    titles
}

/// `.ppt`'s own chunk metadata.
///
/// It used to borrow `.doc`'s `records_to_chunks` wholesale, which meant a
/// presentation described itself in document vocabulary — no `slide_number`,
/// no `slide_title`, no `document_metadata` — while `.pptx` had all three
/// (TECH_DEBT #18). `page_number` is kept alongside `slide_number` because it
/// is the shipped key and they mean the same thing for a deck.
fn ppt_records_to_chunks(
    source: &str,
    records: Vec<ChunkRecord>,
    paragraphs: &[DocParagraph],
) -> Vec<Chunk> {
    let titles = slide_titles(paragraphs);
    let total_slides = paragraphs
        .iter()
        .filter_map(|p| p.page_index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
    let total = records.len();
    records
        .into_iter()
        .map(|c| {
            let slide_number = c.page_number;
            let metadata = serde_json::json!({
                "source": source,
                "chunk_index": c.chunk_index,
                "total_chunks": total,
                "paragraph_type": c.paragraph_type,
                "heading_level": c.heading_level,
                "page_number": slide_number,
                "slide_number": slide_number,
                "slide_title": slide_number.and_then(|n| titles.get(&(n - 1)).cloned()),
                "document_metadata": {
                    "source_type": "ppt",
                    "total_slides": total_slides,
                },
            });
            Chunk::new(c.content, c.content_type, metadata)
        })
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
    validate_ppt_path(file_path).map_err(ChunkError::InvalidArg)?;
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let paragraphs = load_ppt_paragraphs(file_path).map_err(ChunkError::Parse)?;
    let records = build_by_mode(paragraphs.clone(), mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    Ok(ppt_records_to_chunks(file_path, records, &paragraphs))
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
    let records = build_by_mode(paragraphs.clone(), mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    Ok(ppt_records_to_chunks(source, records, &paragraphs))
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
