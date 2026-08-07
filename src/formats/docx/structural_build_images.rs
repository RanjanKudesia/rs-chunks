//! Chunk assembly for the DOCX structural/default mode, image-aware path.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

use super::common::image_hash_name;
use super::structural_build::{
    base_chunk_metadata, collect_element_refs, flush_outside_shorts, flush_section,
};
use super::structural_model::{
    ChunkRecordInput, ContentType, DocumentElement, SEMANTIC_SPLIT_MAX_BYTES,
};
use super::structural_text::semantic_chunks;

pub(super) fn build_chunks_from_elements_with_images(
    elements: Vec<DocumentElement>,
    doc_metadata: &Value,
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
    image_rids_map: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<ChunkRecordInput> {
    let mut chunks = Vec::new();
    let mut section_heading: Option<String> = None;
    let mut section_parts: Vec<DocumentElement> = Vec::new();
    let mut outside_short_parts: Vec<DocumentElement> = Vec::new();
    let mut outside_short_first_page: Option<usize> = None;

    let mut i = 0usize;
    while i < elements.len() {
        let element = &elements[i];

        match element.content_type {
            ContentType::HeaderFooter
            | ContentType::FootnoteCaption
            | ContentType::MixedContent => {}
            ContentType::HeadingSection => {
                flush_outside_shorts(
                    &mut chunks,
                    &mut outside_short_parts,
                    &mut outside_short_first_page,
                    doc_metadata,
                    footnote_map,
                    endnote_map,
                );
                flush_section(
                    &mut chunks,
                    &mut section_heading,
                    &mut section_parts,
                    doc_metadata,
                    footnote_map,
                    endnote_map,
                );
                section_heading = Some(element.text.clone());
                section_parts.push(element.clone());
            }
            ContentType::BulletNumberedList => {
                let mut bullets: Vec<DocumentElement> = vec![element.clone()];
                let mut j = i + 1;
                while j < elements.len()
                    && elements[j].content_type == ContentType::BulletNumberedList
                {
                    bullets.push(elements[j].clone());
                    j += 1;
                }
                let list_text = bullets
                    .iter()
                    .map(|b| b.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");

                if section_heading.is_some() {
                    let mut merged_footnotes: Vec<String> = Vec::new();
                    let mut merged_endnotes: Vec<String> = Vec::new();
                    for b in &bullets {
                        merged_footnotes.extend(b.footnote_refs.iter().cloned());
                        merged_endnotes.extend(b.endnote_refs.iter().cloned());
                    }
                    section_parts.push(DocumentElement {
                        content_type: ContentType::BulletNumberedList,
                        text: list_text,
                        page_number: element.page_number,
                        heading_level: None,
                        footnote_refs: merged_footnotes,
                        endnote_refs: merged_endnotes,
                        image_rid: None,
                    });
                } else {
                    flush_outside_shorts(
                        &mut chunks,
                        &mut outside_short_parts,
                        &mut outside_short_first_page,
                        doc_metadata,
                        footnote_map,
                        endnote_map,
                    );
                    let mut fns = Vec::new();
                    let mut ens = Vec::new();
                    for b in &bullets {
                        collect_element_refs(b, footnote_map, endnote_map, &mut fns, &mut ens);
                    }
                    chunks.push(ChunkRecordInput {
                        content_type: ContentType::BulletNumberedList,
                        content: list_text,
                        metadata: base_chunk_metadata(
                            None,
                            None,
                            &fns,
                            &ens,
                            doc_metadata,
                            element.page_number,
                        ),
                    });
                }
                i = j - 1;
            }
            ContentType::Table => {
                flush_outside_shorts(
                    &mut chunks,
                    &mut outside_short_parts,
                    &mut outside_short_first_page,
                    doc_metadata,
                    footnote_map,
                    endnote_map,
                );
                flush_section(
                    &mut chunks,
                    &mut section_heading,
                    &mut section_parts,
                    doc_metadata,
                    footnote_map,
                    endnote_map,
                );
                let mut fns = Vec::new();
                let mut ens = Vec::new();
                collect_element_refs(element, footnote_map, endnote_map, &mut fns, &mut ens);
                chunks.push(ChunkRecordInput {
                    content_type: ContentType::Table,
                    content: element.text.clone(),
                    metadata: base_chunk_metadata(
                        None,
                        None,
                        &fns,
                        &ens,
                        doc_metadata,
                        element.page_number,
                    ),
                });
            }
            ContentType::CodeBlock => {
                flush_outside_shorts(
                    &mut chunks,
                    &mut outside_short_parts,
                    &mut outside_short_first_page,
                    doc_metadata,
                    footnote_map,
                    endnote_map,
                );
                flush_section(
                    &mut chunks,
                    &mut section_heading,
                    &mut section_parts,
                    doc_metadata,
                    footnote_map,
                    endnote_map,
                );
                let mut fns = Vec::new();
                let mut ens = Vec::new();
                collect_element_refs(element, footnote_map, endnote_map, &mut fns, &mut ens);
                chunks.push(ChunkRecordInput {
                    content_type: ContentType::CodeBlock,
                    content: element.text.clone(),
                    metadata: base_chunk_metadata(
                        None,
                        None,
                        &fns,
                        &ens,
                        doc_metadata,
                        element.page_number,
                    ),
                });
            }
            ContentType::ShortDisconnectedParagraph => {
                if section_heading.is_some() {
                    section_parts.push(element.clone());
                } else {
                    if outside_short_parts.is_empty() {
                        outside_short_first_page = element.page_number;
                    }
                    outside_short_parts.push(element.clone());
                }
            }
            ContentType::PlainParagraph | ContentType::LongSingleParagraph => {
                if section_heading.is_some() {
                    section_parts.push(element.clone());
                } else {
                    flush_outside_shorts(
                        &mut chunks,
                        &mut outside_short_parts,
                        &mut outside_short_first_page,
                        doc_metadata,
                        footnote_map,
                        endnote_map,
                    );
                    let split = semantic_chunks(&element.text, SEMANTIC_SPLIT_MAX_BYTES);
                    let mut fns = Vec::new();
                    let mut ens = Vec::new();
                    collect_element_refs(element, footnote_map, endnote_map, &mut fns, &mut ens);
                    for (idx, s) in split.into_iter().enumerate() {
                        let (chunk_fns, chunk_ens): (&[(String, String)], &[(String, String)]) =
                            if idx == 0 {
                                (fns.as_slice(), ens.as_slice())
                            } else {
                                (&[], &[])
                            };
                        chunks.push(ChunkRecordInput {
                            content_type: element.content_type,
                            content: s,
                            metadata: base_chunk_metadata(
                                None,
                                None,
                                chunk_fns,
                                chunk_ens,
                                doc_metadata,
                                element.page_number,
                            ),
                        });
                    }
                }
            }
            ContentType::Image => {
                if section_heading.is_some() {
                    // Inside a section: emit a standalone image chunk immediately,
                    // then push the element to section_parts so the section text
                    // is identical to the list_images=False path.
                    if let Some(rid) = element.image_rid.as_deref() {
                        if let Some(zip_path) = image_rids_map.get(rid) {
                            if let Ok(mut entry) = archive.by_name(zip_path) {
                                let mut bytes = Vec::new();
                                if entry.read_to_end(&mut bytes).is_ok() {
                                    if let Some(hash_name) = image_hash_name(&bytes, zip_path) {
                                        if !image_out.iter().any(|(n, _)| n == &hash_name) {
                                            image_out.push((hash_name.clone(), bytes));
                                        }
                                        chunks.push(ChunkRecordInput {
                                            content_type: ContentType::Image,
                                            content: hash_name.clone(),
                                            metadata: json!({
                                                "image_name": hash_name,
                                                "alt_text": element.text
                                                    .strip_prefix("[Image: ")
                                                    .and_then(|s| s.strip_suffix(']'))
                                                    .unwrap_or(""),
                                            }),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    section_parts.push(element.clone());
                } else {
                    // Outside section: flush accumulated short paragraphs first,
                    // then emit image chunk.
                    flush_outside_shorts(
                        &mut chunks,
                        &mut outside_short_parts,
                        &mut outside_short_first_page,
                        doc_metadata,
                        footnote_map,
                        endnote_map,
                    );
                    if let Some(rid) = element.image_rid.as_deref() {
                        if let Some(zip_path) = image_rids_map.get(rid) {
                            if let Ok(mut entry) = archive.by_name(zip_path) {
                                let mut bytes = Vec::new();
                                if entry.read_to_end(&mut bytes).is_ok() {
                                    if let Some(hash_name) = image_hash_name(&bytes, zip_path) {
                                        if !image_out.iter().any(|(n, _)| n == &hash_name) {
                                            image_out.push((hash_name.clone(), bytes));
                                        }
                                        chunks.push(ChunkRecordInput {
                                            content_type: ContentType::Image,
                                            content: hash_name.clone(),
                                            metadata: json!({
                                                "image_name": hash_name,
                                                "alt_text": element.text
                                                    .strip_prefix("[Image: ")
                                                    .and_then(|s| s.strip_suffix(']'))
                                                    .unwrap_or(""),
                                            }),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    // Unsupported format (.emf etc.) or missing rid — skip silently
                }
            }
        }

        i += 1;
    }

    flush_outside_shorts(
        &mut chunks,
        &mut outside_short_parts,
        &mut outside_short_first_page,
        doc_metadata,
        footnote_map,
        endnote_map,
    );
    flush_section(
        &mut chunks,
        &mut section_heading,
        &mut section_parts,
        doc_metadata,
        footnote_map,
        endnote_map,
    );

    chunks
}
