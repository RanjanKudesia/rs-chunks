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
// Path-based twin of the used `_bytes` variant, kept for loader API symmetry.
/// Main-story paragraphs with their ordinals, plus the side stories.
///
/// Both halves matter: the ordinals anchor inline images, and the side stories
/// are the footnotes, headers, comments, endnotes and text boxes that
/// `to_markdown` has always included. Returning only the first half is what
/// made the images surface lose prose (L5).
pub(crate) fn load_doc_paragraphs_indexed_bytes(
    bytes: &[u8],
) -> Result<super::loader::IndexedAndSideStories, String> {
    ParsedDoc::open(bytes)?.all_paragraphs_indexed()
}
