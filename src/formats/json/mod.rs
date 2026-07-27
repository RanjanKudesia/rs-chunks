//! JSON / JSONL / NDJSON chunking. Records are rendered to Markdown then chunked
//! by the shared markdown pipeline.

pub mod extract;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use extract::{parse_document, parse_lines, JsonKind};

fn kind_for(file_path: &str) -> Result<JsonKind> {
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
        Ok(JsonKind::Lines)
    } else if lower.ends_with(".json") {
        Ok(JsonKind::Document)
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .json, .jsonl or .ndjson file path, got: {file_path}"
        )))
    }
}

fn source_type(file_path: &str) -> &'static str {
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".ndjson") {
        "ndjson"
    } else if lower.ends_with(".jsonl") {
        "jsonl"
    } else {
        "json"
    }
}

fn load(file_path: &str) -> Result<Loaded> {
    let raw = std::fs::read(file_path).map_err(ChunkError::Io)?;
    load_bytes(&raw, file_path)
}

fn load_bytes(raw: &[u8], filename: &str) -> Result<Loaded> {
    let kind = kind_for(filename)?;
    let doc = match kind {
        JsonKind::Lines => parse_lines(raw),
        JsonKind::Document => parse_document(raw).map_err(ChunkError::Parse)?,
    };
    let metadata = serde_json::json!({
        "source_type": source_type(filename),
        "record_count": doc.record_count,
        "top_level": doc.top_level,
        "envelope_key": doc.envelope_key,
    });
    Ok(Loaded { markdown: doc.markdown, images: Vec::new(), metadata })
}

/// No-filesystem entry (wasm/browser + bytes callers). `filename` routes the
/// JSON kind (`.json` document vs `.jsonl`/`.ndjson` lines).
pub fn chunk_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    pipeline::chunk(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    Ok(load_bytes(data, filename)?.markdown)
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
