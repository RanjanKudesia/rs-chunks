//! DOCX structural/default mode: entry points.
//!
//! The implementation lives in focused sibling modules — `structural_model`
//! (constants, element model, parsing), `structural_build` /
//! `structural_build_images` (chunk assembly), `structural_text` (splitting)
//! and `docx_aux` (notes/zip helpers). This module keeps the two entry points
//! and re-exports the helpers other DOCX modes use.

pub(super) use super::docx_aux::{extract_notes_map, read_zip_entry};
use super::structural_build::build_chunks_from_elements;
use super::structural_build_images::build_chunks_from_elements_with_images;
use super::structural_model::parse_docx_document;

/// Native entry: parse + structural/default chunking → public chunks.
pub(super) fn chunk(bytes: &[u8]) -> Result<Vec<crate::chunk::Chunk>, String> {
    let parsed = parse_docx_document(bytes)?;
    let recs = build_chunks_from_elements(
        parsed.elements,
        &parsed.doc_metadata,
        &parsed.footnote_map,
        &parsed.endnote_map,
    );
    Ok(recs
        .into_iter()
        .map(|c| crate::chunk::Chunk::new(c.content, c.content_type.as_str(), c.metadata))
        .collect())
}

pub(super) fn chunk_with_images(bytes: &[u8]) -> Result<crate::chunk::ChunksWithImages, String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let parsed = parse_docx_document(bytes)?;
    let mut image_out = Vec::new();
    let combined = build_chunks_from_elements_with_images(
        parsed.elements,
        &parsed.doc_metadata,
        &parsed.footnote_map,
        &parsed.endnote_map,
        &image_rids_map,
        &mut archive,
        &mut image_out,
    );
    Ok((
        combined
            .into_iter()
            .map(|c| crate::chunk::Chunk::new(c.content, c.content_type.as_str(), c.metadata))
            .collect(),
        image_out,
    ))
}
