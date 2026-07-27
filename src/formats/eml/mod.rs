//! `.eml` / `.mbox` email chunking (markdown pipeline, with inline images).

pub mod extract;
pub mod mbox;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use extract::{document_to_markdown, parse_message_bytes};
use mbox::mbox_to_markdown;

fn ensure_email(file_path: &str) -> Result<()> {
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".eml") || lower.ends_with(".mbox") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .eml or .mbox file path, got: {file_path}"
        )))
    }
}

fn load(file_path: &str) -> Result<Loaded> {
    ensure_email(file_path)?;
    let raw = std::fs::read(file_path).map_err(ChunkError::Io)?;
    load_bytes(&raw, file_path)
}

/// No-filesystem entry (wasm/browser). `filename` routes `.eml` vs `.mbox`.
pub fn chunk_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    pipeline::chunk(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    Ok(load_bytes(data, filename)?.markdown)
}

fn load_bytes(raw: &[u8], file_path: &str) -> Result<Loaded> {
    if file_path.to_ascii_lowercase().ends_with(".mbox") {
        let (markdown, images, count) = mbox_to_markdown(raw);
        let metadata = serde_json::json!({ "source_type": "mbox", "message_count": count });
        Ok(Loaded { markdown, images, metadata })
    } else {
        let doc = parse_message_bytes(raw);
        let markdown = document_to_markdown(&doc, 1);
        let metadata = serde_json::json!({
            "source_type": "eml",
            "subject": doc.subject,
            "from": doc.from,
            "to": doc.to,
            "cc": doc.cc,
            "bcc": doc.bcc,
            "date": doc.date,
            "message_id": doc.message_id,
            "has_attachments": !doc.attachments.is_empty(),
            "attachment_count": doc.attachments.len(),
        });
        Ok(Loaded { markdown, images: doc.images, metadata })
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
    pipeline::chunk(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    pipeline::chunk_opts(&load(file_path)?, opts)
}

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(load(file_path)?.markdown)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load(file_path)?;
    Ok((l.markdown, crate::formats::pipeline::dedup_images(l.images)))
}

/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_with_images_from_bytes(data: &[u8], filename: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load_bytes(data, filename)?;
    Ok((l.markdown, crate::formats::pipeline::dedup_images(l.images)))
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
