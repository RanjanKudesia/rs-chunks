//! Loading a `.doc` into the paragraph stream the builders chunk.
//!
//! The builders themselves live in [`super::builders`] — they are shared with
//! `.ppt`, which produces the same `DocParagraph` list from a different file
//! format.

use std::path::Path;

use super::loader::ParsedDoc;
use super::text_extractor::DocParagraph;

pub(crate) use super::builders::{
    build_page_aware_chunks, build_section_chunks, build_semantic_chunks, build_sentence_chunks,
    build_sliding_window_chunks, build_structural_chunks, ChunkRecord,
};

pub(crate) fn validate_doc_path(file_path: &str) -> Result<(), String> {
    if !file_path.to_ascii_lowercase().ends_with(".doc") {
        return Err(format!("Expected .doc file path, got: {file_path}"));
    }
    if !Path::new(file_path).exists() {
        return Err(format!("File not found: {file_path}"));
    }
    Ok(())
}

pub(crate) fn load_doc_paragraphs(file_path: &str) -> Result<Vec<DocParagraph>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    load_doc_paragraphs_bytes(&bytes)
}

pub(crate) fn load_doc_paragraphs_bytes(bytes: &[u8]) -> Result<Vec<DocParagraph>, String> {
    ParsedDoc::open(bytes)?.all_paragraphs()
}

/// Like [`load_doc_paragraphs`] but keeps each paragraph's raw ordinal, so
/// callers can interleave content anchored by raw paragraph index (used by
/// the image-aware markdown converter).
pub(crate) fn load_doc_paragraphs_indexed(
    file_path: &str,
) -> Result<Vec<(usize, DocParagraph)>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    load_doc_paragraphs_indexed_bytes(&bytes)
}

pub(crate) fn load_doc_paragraphs_indexed_bytes(
    bytes: &[u8],
) -> Result<Vec<(usize, DocParagraph)>, String> {
    ParsedDoc::open(bytes)?.main_paragraphs_indexed()
}
