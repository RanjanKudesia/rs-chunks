//! Legacy Word binary (`.doc`, OLE/CFB) chunking. All modes load the paragraph
//! list from the WordDocument stream, then build chunks.

pub mod cfb_reader;
pub mod fib;
pub mod images;
pub mod paragraph_props;
pub mod piece_table;
pub mod structural;
pub mod stylesheet;
pub mod text_extractor;
pub mod to_markdown;

use serde_json::json;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use structural::{
    build_page_aware_chunks, build_section_chunks, build_semantic_chunks, build_sentence_chunks,
    build_sliding_window_chunks, build_structural_chunks, load_doc_paragraphs, validate_doc_path,
    ChunkRecord,
};

pub(crate) fn records_to_chunks(file_path: &str, records: Vec<ChunkRecord>) -> Vec<Chunk> {
    let total = records.len();
    records
        .into_iter()
        .map(|c| {
            let metadata = json!({
                "source": file_path,
                "chunk_index": c.chunk_index,
                "total_chunks": total,
                "paragraph_type": c.paragraph_type,
                "heading_level": c.heading_level,
                // 1-based hard-page number, or null when the document declares
                // no page breaks at all (TECH_DEBT #11).
                "page_number": c.page_number,
            });
            Chunk::new(c.content, c.content_type, metadata)
        })
        .collect()
}

/// Reject the argument combinations a mode cannot express, *before* any
/// parsing work happens.
///
/// The builders themselves degrade to an empty `Vec` on bad arguments, which
/// silently turns a caller mistake into "this document has no content".
/// py_chunks has always raised instead, and that is the better behaviour, so it
/// moves into the engine rather than staying in the binding
/// (CONSOLIDATION_PLAN.md §4). `.ppt` calls this too — it reuses these builders.
pub(crate) fn validate_mode_args(
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<()> {
    let bad = |m: &str| Err(ChunkError::InvalidArg(m.to_string()));
    match mode {
        "sliding_window" if window_size == 0 => bad("window_size must be greater than 0"),
        "sliding_window" if overlap >= window_size => bad("overlap must be less than window_size"),
        "sentence" if sentences_per_chunk == 0 => bad("sentences_per_chunk must be greater than 0"),
        "page_aware" if paragraphs_per_page == 0 => bad("paragraphs_per_page must be greater than 0"),
        _ => Ok(()),
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
    validate_doc_path(file_path).map_err(ChunkError::InvalidArg)?;
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let paragraphs = load_doc_paragraphs(file_path).map_err(ChunkError::Parse)?;
    let records = build_by_mode(paragraphs, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    Ok(records_to_chunks(file_path, records))
}

fn build_by_mode(
    paragraphs: Vec<text_extractor::DocParagraph>,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<ChunkRecord>> {
    Ok(match mode {
        "default" | "structural" => build_structural_chunks(paragraphs),
        "section" => build_section_chunks(paragraphs),
        "semantic" => build_semantic_chunks(paragraphs),
        "sentence" => build_sentence_chunks(paragraphs, sentences_per_chunk),
        "page_aware" => build_page_aware_chunks(paragraphs, paragraphs_per_page),
        "sliding_window" => build_sliding_window_chunks(paragraphs, window_size, overlap),
        other => return Err(ChunkError::InvalidArg(format!("Unknown DOC mode: {other}"))),
    })
}

/// No-filesystem entry (wasm/browser). `source` label is used for the `source`
/// metadata field (pass the filename).
pub fn chunk_from_bytes(data: &[u8], source: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let paragraphs = structural::load_doc_paragraphs_bytes(data).map_err(ChunkError::Parse)?;
    let records = build_by_mode(paragraphs, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    Ok(records_to_chunks(source, records))
}

pub fn to_markdown_from_bytes(data: &[u8]) -> Result<String> {
    let paragraphs = structural::load_doc_paragraphs_bytes(data).map_err(ChunkError::Parse)?;
    Ok(to_markdown::render_paragraphs_markdown(paragraphs))
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
                "DOC does not support mode '{}'",
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
    validate_doc_path(file_path).map_err(ChunkError::InvalidArg)?;
    validate_mode_args(mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?;
    let res = match mode {
        "default" | "structural" => images::chunk_with_images_impl(file_path, build_structural_chunks),
        "section" => images::chunk_with_images_impl(file_path, build_section_chunks),
        "semantic" => images::chunk_with_images_impl(file_path, build_semantic_chunks),
        "sentence" => images::chunk_with_images_impl(file_path, |p| build_sentence_chunks(p, sentences_per_chunk)),
        "page_aware" => images::chunk_with_images_impl(file_path, |p| build_page_aware_chunks(p, paragraphs_per_page)),
        "sliding_window" => images::chunk_with_images_impl(file_path, |p| build_sliding_window_chunks(p, window_size, overlap)),
        other => return Err(ChunkError::InvalidArg(format!("Unknown DOC mode: {other}"))),
    };
    res.map_err(ChunkError::Parse)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    validate_doc_path(file_path).map_err(ChunkError::InvalidArg)?;
    let paragraphs = load_doc_paragraphs(file_path).map_err(ChunkError::Parse)?;
    Ok(to_markdown::render_paragraphs_markdown(paragraphs))
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
        other => return Err(ChunkError::InvalidArg(format!("Unknown DOC mode: {other}"))),
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
