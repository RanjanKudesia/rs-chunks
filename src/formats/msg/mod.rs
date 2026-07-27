//! `.msg` (Outlook) email chunking (markdown pipeline).

pub mod extract;
pub mod nameid;
pub mod rtf;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use extract::{document_to_markdown, extract_document};

fn ensure_msg(file_path: &str) -> Result<()> {
    if file_path.to_ascii_lowercase().ends_with(".msg") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .msg file path, got: {file_path}"
        )))
    }
}

fn load(file_path: &str) -> Result<Loaded> {
    ensure_msg(file_path)?;
    let doc = extract_document(file_path).map_err(ChunkError::Parse)?;
    loaded_from_doc(doc)
}

fn load_bytes(data: &[u8]) -> Result<Loaded> {
    let doc = extract::extract_document_bytes(data).map_err(ChunkError::Parse)?;
    loaded_from_doc(doc)
}

/// No-filesystem entry (wasm/browser).
pub fn chunk_from_bytes(data: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    pipeline::chunk(&load_bytes(data)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8]) -> Result<String> {
    Ok(load_bytes(data)?.markdown)
}

fn loaded_from_doc(doc: extract::MsgDocument) -> Result<Loaded> {
    let markdown = document_to_markdown(&doc);
    let metadata = serde_json::json!({
        "source_type": "msg",
        "message_class": doc.message_class,
        "subject": doc.subject,
        "from": doc.from,
        "to": doc.to,
        "cc": doc.cc,
        "bcc": doc.bcc,
        "sent_date": doc.sent_date,
        "received_date": doc.received_date,
        "importance": doc.importance,
        "conversation_topic": doc.conversation_topic,
        "has_attachments": !doc.attachments.is_empty(),
        "attachment_count": doc.attachments.len(),
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
