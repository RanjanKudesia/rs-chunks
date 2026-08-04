//! `.eml` / `.mbox` email chunking (markdown pipeline, with inline images).

pub mod extract;
pub mod mbox;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use extract::{document_to_markdown, parse_message_bytes};
use mbox::{mbox_to_markdown, MboxMessageInfo};

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

/// Give each `.mbox` chunk the identity of the message it came from.
///
/// Same shape as the `.odp` slide pass: the `## Message N` heading marks the
/// boundary. A 152-message mailbox used to give every chunk the same
/// `{source_type, message_count}` — the per-message envelope was parsed and
/// thrown away, so "which message is this?" was unanswerable. (#36)
fn inject_message_metadata(chunks: &mut [Chunk], infos: &[MboxMessageInfo]) {
    let by_index: std::collections::HashMap<u64, &MboxMessageInfo> =
        infos.iter().map(|i| (i.index as u64, i)).collect();
    let mut current: Option<u64> = None;

    for chunk in chunks.iter_mut() {
        if let Some(n) = message_number_of(chunk.content.trim()) {
            current = Some(n);
        } else if let Some(n) = chunk
            .metadata
            .get("section_heading")
            .and_then(|v| v.as_str())
            .and_then(message_number_of)
        {
            current = Some(n);
        }
        let Some(n) = current else { continue };
        let Some(info) = by_index.get(&n) else { continue };
        if let Some(map) = chunk.metadata.as_object_mut() {
            map.insert("message_index".into(), serde_json::json!(n));
            map.insert("message_subject".into(), serde_json::json!(info.subject));
            map.insert("message_from".into(), serde_json::json!(info.from));
            map.insert("message_date".into(), serde_json::json!(info.date));
            map.insert("message_id".into(), serde_json::json!(info.message_id));
            map.insert("in_reply_to".into(), serde_json::json!(info.in_reply_to));
            map.insert("references".into(), serde_json::json!(info.references));
        }
    }
}

/// `"Message 7"` -> `Some(7)`.
fn message_number_of(text: &str) -> Option<u64> {
    text.strip_prefix("Message ")?.trim().parse().ok()
}

/// `pipeline::chunk` plus the per-message identity a mailbox needs. `.eml` is a
/// single message, so `infos` is empty and the pass is a no-op.
fn chunk_mbox(
    loaded_and_infos: &(Loaded, Vec<MboxMessageInfo>),
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let (loaded, infos) = loaded_and_infos;
    let mut chunks = pipeline::chunk(
        loaded,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    if !infos.is_empty() {
        inject_message_metadata(&mut chunks, infos);
    }
    Ok(chunks)
}

fn load(file_path: &str) -> Result<(Loaded, Vec<MboxMessageInfo>)> {
    ensure_email(file_path)?;
    let raw = std::fs::read(file_path).map_err(ChunkError::Io)?;
    load_bytes(&raw, file_path)
}

/// No-filesystem entry (wasm/browser). `filename` routes `.eml` vs `.mbox`.
pub fn chunk_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    chunk_mbox(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    Ok(load_bytes(data, filename)?.0.markdown)
}

fn load_bytes(raw: &[u8], file_path: &str) -> Result<(Loaded, Vec<MboxMessageInfo>)> {
    if file_path.to_ascii_lowercase().ends_with(".mbox") {
        let (markdown, images, count, infos) = mbox_to_markdown(raw);
        let metadata = serde_json::json!({ "source_type": "mbox", "message_count": count });
        Ok((Loaded { markdown, images, metadata }, infos))
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
            "in_reply_to": doc.in_reply_to,
            "references": doc.references,
            "has_attachments": !doc.attachments.is_empty(),
            "attachment_count": doc.attachments.len(),
        });
        Ok((Loaded { markdown, images: doc.images, metadata }, Vec::new()))
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
    chunk_mbox(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    pipeline::chunk_opts(&load(file_path)?.0, opts)
}

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load(file_path)?.0, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(load(file_path)?.0.markdown)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load(file_path)?.0;
    Ok((l.markdown, crate::formats::pipeline::dedup_images(l.images)))
}

/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load_bytes(data, filename)?.0, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_with_images_from_bytes(data: &[u8], filename: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load_bytes(data, filename)?.0;
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
