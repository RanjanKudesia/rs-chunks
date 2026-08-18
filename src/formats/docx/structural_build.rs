//! Chunk assembly for the DOCX structural/default mode (text-only path),
//! plus the flush/metadata helpers shared with the image-aware path.

use serde_json::{json, Value};
use std::collections::HashMap;

use super::structural_model::{
    ChunkRecordInput, ContentType, DocumentElement, MAX_SECTION_CHARS, SEMANTIC_SPLIT_MAX_BYTES,
    SHORT_AGGREGATE_CHUNK_OVERLAP, SHORT_AGGREGATE_CHUNK_SIZE,
};
use super::structural_text::{recursive_char_chunks, semantic_chunks};

/// The `(footnotes, endnotes)` a single sub-chunk carries. Both are `(id, text)`
/// pairs, and both are empty for every sub-chunk after the first — see the
/// call sites for why duplication is avoided.
pub(super) type NoteSlices<'a> = (&'a [(String, String)], &'a [(String, String)]);

pub(super) fn build_chunks_from_elements(
    elements: Vec<DocumentElement>,
    doc_metadata: &Value,
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
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
                    // Merge bullet refs into a single synthetic element so
                    // the section flush sees them as one logical part.
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
            ContentType::PlainParagraph | ContentType::LongSingleParagraph | ContentType::Image => {
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
                        // Attach the element's footnotes/endnotes only to the
                        // first sub-chunk; later sub-chunks of the same
                        // paragraph carry empty arrays to avoid duplication.
                        let (chunk_fns, chunk_ens): NoteSlices = if idx == 0 {
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

pub(super) fn flush_outside_shorts(
    chunks: &mut Vec<ChunkRecordInput>,
    outside_short_parts: &mut Vec<DocumentElement>,
    outside_short_first_page: &mut Option<usize>,
    doc_metadata: &Value,
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
) {
    if outside_short_parts.is_empty() {
        return;
    }

    let merged = outside_short_parts
        .iter()
        .map(|p| p.text.clone())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let mut fns = Vec::new();
    let mut ens = Vec::new();
    for p in outside_short_parts.iter() {
        collect_element_refs(p, footnote_map, endnote_map, &mut fns, &mut ens);
    }
    outside_short_parts.clear();
    let page = outside_short_first_page.take();
    if merged.is_empty() {
        return;
    }

    for (idx, item) in recursive_char_chunks(
        &merged,
        SHORT_AGGREGATE_CHUNK_SIZE,
        SHORT_AGGREGATE_CHUNK_OVERLAP,
    )
    .into_iter()
    .enumerate()
    {
        let (chunk_fns, chunk_ens): NoteSlices = if idx == 0 {
            (fns.as_slice(), ens.as_slice())
        } else {
            (&[], &[])
        };
        chunks.push(ChunkRecordInput {
            content_type: ContentType::ShortDisconnectedParagraph,
            content: item,
            metadata: base_chunk_metadata(None, None, chunk_fns, chunk_ens, doc_metadata, page),
        });
    }
}

pub(super) fn flush_section(
    chunks: &mut Vec<ChunkRecordInput>,
    section_heading: &mut Option<String>,
    section_parts: &mut Vec<DocumentElement>,
    doc_metadata: &Value,
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
) {
    if section_parts.is_empty() {
        return;
    }

    let heading = section_heading.clone();
    let heading_level = section_parts.first().and_then(|p| p.heading_level);
    let section_page = section_parts.first().and_then(|p| p.page_number);
    let mut has_paragraph = false;
    let mut has_bullets = false;
    let mut has_image = false;
    let mut lines = Vec::new();
    let mut shorts = Vec::new();
    let mut fns = Vec::new();
    let mut ens = Vec::new();

    for part in section_parts.iter() {
        collect_element_refs(part, footnote_map, endnote_map, &mut fns, &mut ens);
        match part.content_type {
            ContentType::BulletNumberedList => {
                has_bullets = true;
                lines.push(part.text.clone());
            }
            ContentType::HeadingSection => {
                lines.push(part.text.clone());
            }
            ContentType::ShortDisconnectedParagraph => {
                has_paragraph = true;
                shorts.push(part.text.clone());
            }
            ContentType::Image => {
                has_image = true;
                lines.push(part.text.clone());
            }
            _ => {
                has_paragraph = true;
                lines.push(part.text.clone());
            }
        }
    }

    if !shorts.is_empty() {
        lines.push(shorts.join(" "));
    }

    let combined = lines.join("\n").trim().to_string();
    if !combined.is_empty() {
        let content_type = if heading.is_some() && (has_paragraph || has_bullets || has_image) {
            ContentType::MixedContent
        } else {
            ContentType::HeadingSection
        };

        // A section is every element between one heading and the next, so it
        // grows without limit — `poi_bug59058.docx` produced an 18,546-character
        // chunk. #68 bounded the md pipeline, txt and html; docx was left out
        // because this is an assembled section rather than a block, and that is
        // the only reason. Splitting here keeps the section model intact: the
        // heading association is metadata, not position, so every part carries
        // the same heading, level, page and notes (TECH_DEBT #91).
        //
        // Tables are not affected — they never enter `section_parts`; the table
        // arm flushes the pending section and pushes its own chunk. `table` is
        // documented as "kept whole", the same rule that protects CSV rows.
        for part in crate::shared::split_block_on_lines_and_sentences(&combined, MAX_SECTION_CHARS)
        {
            chunks.push(ChunkRecordInput {
                content_type,
                content: part,
                metadata: base_chunk_metadata(
                    heading.clone(),
                    heading_level,
                    &fns,
                    &ens,
                    doc_metadata,
                    section_page,
                ),
            });
        }
    }

    section_parts.clear();
    *section_heading = None;
}

pub(super) fn base_chunk_metadata(
    section_heading: Option<String>,
    section_heading_level: Option<u32>,
    footnotes: &[(String, String)],
    endnotes: &[(String, String)],
    doc_metadata: &Value,
    page_number: Option<usize>,
) -> Value {
    let fn_arr: Vec<Value> = footnotes
        .iter()
        .map(|(id, text)| json!({ "id": id, "text": text }))
        .collect();
    let en_arr: Vec<Value> = endnotes
        .iter()
        .map(|(id, text)| json!({ "id": id, "text": text }))
        .collect();
    json!({
        "footnotes": fn_arr,
        "endnotes": en_arr,
        "page_number": page_number,
        "section_heading": section_heading,
        "section_heading_level": section_heading_level,
        "document_metadata": doc_metadata,
    })
}

/// Resolve the footnote/endnote ids stored on `element` against the document
/// maps and append the resulting `(id, text)` pairs to the supplied
/// accumulators. Ids that don't resolve (e.g. references into a stripped
/// separator entry, or a malformed DOCX) are silently dropped.
pub(super) fn collect_element_refs(
    element: &DocumentElement,
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
    footnotes_out: &mut Vec<(String, String)>,
    endnotes_out: &mut Vec<(String, String)>,
) {
    for id in &element.footnote_refs {
        if let Some(text) = footnote_map.get(id) {
            footnotes_out.push((id.clone(), text.clone()));
        }
    }
    for id in &element.endnote_refs {
        if let Some(text) = endnote_map.get(id) {
            endnotes_out.push((id.clone(), text.clone()));
        }
    }
}
