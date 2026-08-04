use quick_xml::escape::unescape;
use serde_json::{json, Value};

use super::common::{
    docx_heading_level, image_hash_name, image_placeholder, parse_docx_blocks, DocxBlock, DocxBlockKind,
};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const MAX_DOCX_AUX_XML_BYTES: u64 = 10 * 1024 * 1024;

/// A paragraph whose visible text length (in bytes) is at or above this
/// threshold is classified as `LongSingleParagraph` and will be split with
/// `semantic_chunks` before emission.
const LONG_PARAGRAPH_THRESHOLD: usize = 500;

/// A paragraph whose visible text length (in bytes) is strictly below this
/// threshold is classified as `ShortDisconnectedParagraph` and aggregated
/// with neighbouring shorts before emission.
const SHORT_PARAGRAPH_THRESHOLD: usize = 80;

/// Maximum target size (in bytes) for one semantic sub-chunk when splitting
/// a long paragraph or image-caption block.
const SEMANTIC_SPLIT_MAX_BYTES: usize = 900;

/// Target size (in bytes) for one chunk when aggregating short disconnected
/// paragraphs via `recursive_char_chunks`.
const SHORT_AGGREGATE_CHUNK_SIZE: usize = 700;

/// Character overlap between successive `recursive_char_chunks` outputs when
/// aggregating short disconnected paragraphs.
const SHORT_AGGREGATE_CHUNK_OVERLAP: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentType {
    PlainParagraph,
    HeadingSection,
    BulletNumberedList,
    Table,
    MixedContent,
    CodeBlock,
    FootnoteCaption,
    Image,
    LongSingleParagraph,
    ShortDisconnectedParagraph,
    HeaderFooter,
}

#[derive(Debug, Clone)]
struct DocumentElement {
    content_type: ContentType,
    text: String,
    page_number: Option<usize>,
    /// Resolved heading level (1-based, where 1 is the highest) for elements
    /// classified as `HeadingSection`. Computed by
    /// [`docx_heading_level`] from the source paragraph's `<w:pStyle>` and
    /// `<w:outlineLvl>` so the same English / French / German style names
    /// and outline fallback used by the other DOCX chunkers apply here too.
    /// `None` for non-heading elements.
    heading_level: Option<u32>,
    /// IDs of footnotes (`word/footnotes.xml`) referenced by this element's
    /// source paragraph, in document order. Empty for elements derived from
    /// tables or paragraphs without any `<w:footnoteReference>`.
    footnote_refs: Vec<String>,
    /// IDs of endnotes (`word/endnotes.xml`) referenced by this element's
    /// source paragraph.
    endnote_refs: Vec<String>,
    /// Relationship ID (`r:embed`) of the image contained in this element.
    /// `None` for non-image elements or when the rId was not captured.
    image_rid: Option<String>,
}

#[derive(Debug, Clone)]
struct DocParseResult {
    elements: Vec<DocumentElement>,
    doc_metadata: Value,
    /// `w:id` → resolved footnote text, excluding separator / continuation
    /// auto-entries.
    footnote_map: HashMap<String, String>,
    /// `w:id` → resolved endnote text.
    endnote_map: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content_type: ContentType,
    content: String,
    metadata: Value,
}

impl ContentType {
    fn as_str(self) -> &'static str {
        match self {
            ContentType::PlainParagraph => "plain_paragraph",
            ContentType::HeadingSection => "heading",
            ContentType::BulletNumberedList => "bullet_list",
            ContentType::Table => "table",
            ContentType::MixedContent => "mixed_content",
            ContentType::CodeBlock => "code_block",
            ContentType::FootnoteCaption => "footnote_caption",
            ContentType::Image => "image",
            ContentType::LongSingleParagraph => "long_single_paragraph",
            ContentType::ShortDisconnectedParagraph => "short_disconnected_paragraph",
            ContentType::HeaderFooter => "header_footer",
        }
    }
}

fn parse_docx_document(bytes: &[u8]) -> Result<DocParseResult, String> {
    let raw_blocks = parse_docx_blocks(bytes)?;
    let mut elements = lower_blocks_to_elements(raw_blocks);

    let cursor = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| format!("DOCX is not a valid zip archive: {e}"))?;

    let footnotes_xml = read_zip_entry(&mut archive, "word/footnotes.xml", MAX_DOCX_AUX_XML_BYTES)?;
    let endnotes_xml = read_zip_entry(&mut archive, "word/endnotes.xml", MAX_DOCX_AUX_XML_BYTES)?;
    let header_xml =
        read_first_prefixed_entry(&mut archive, "word/header", MAX_DOCX_AUX_XML_BYTES)?;
    let footer_xml =
        read_first_prefixed_entry(&mut archive, "word/footer", MAX_DOCX_AUX_XML_BYTES)?;
    let image_count = count_prefixed_entries(&mut archive, "word/media/")?;

    let footnote_map = footnotes_xml
        .as_deref()
        .map(|x| extract_notes_map(x, "footnote"))
        .unwrap_or_default();
    let endnote_map = endnotes_xml
        .as_deref()
        .map(|x| extract_notes_map(x, "endnote"))
        .unwrap_or_default();

    if let Some(header_text) = header_xml
        .as_ref()
        .and_then(|x| extract_text_from_xml(x).ok())
        .filter(|x| !x.trim().is_empty())
    {
        elements.push(DocumentElement {
            content_type: ContentType::HeaderFooter,
            text: header_text,
            page_number: None,
            heading_level: None,
            footnote_refs: Vec::new(),
            endnote_refs: Vec::new(),
            image_rid: None,
        });
    }

    if let Some(footer_text) = footer_xml
        .as_ref()
        .and_then(|x| extract_text_from_xml(x).ok())
        .filter(|x| !x.trim().is_empty())
    {
        elements.push(DocumentElement {
            content_type: ContentType::HeaderFooter,
            text: footer_text,
            page_number: None,
            heading_level: None,
            footnote_refs: Vec::new(),
            endnote_refs: Vec::new(),
            image_rid: None,
        });
    }

    let doc_metadata = json!({
        "header_text": header_xml.and_then(|x| extract_text_from_xml(&x).ok()),
        "footer_text": footer_xml.and_then(|x| extract_text_from_xml(&x).ok()),
        "image_count": image_count,
    });

    Ok(DocParseResult {
        elements,
        doc_metadata,
        footnote_map,
        endnote_map,
    })
}

fn lower_blocks_to_elements(raw: Vec<DocxBlock>) -> Vec<DocumentElement> {
    let mut out: Vec<DocumentElement> = Vec::with_capacity(raw.len());
    let mut current_page: usize = 1;

    for block in raw {
        let block_page = current_page;
        let triggers_page_break =
            block.page_break || block.section_break || block.rendered_page_break;

        match block.kind {
            DocxBlockKind::Table => {
                let table_text = block.text.trim().to_string();
                if !table_text.is_empty() {
                    out.push(DocumentElement {
                        content_type: ContentType::Table,
                        text: table_text,
                        page_number: Some(block_page),
                        heading_level: None,
                        footnote_refs: block.footnote_refs.clone(),
                        endnote_refs: block.endnote_refs.clone(),
                        image_rid: None,
                    });
                }
            }
            DocxBlockKind::Paragraph => {
                let text = block.text.trim().to_string();
                let has_text = !text.is_empty();
                let heading_level =
                    docx_heading_level(block.heading_style.as_deref(), block.outline_level);
                let content_type = classify_paragraph_content(
                    &text,
                    block.heading_style.as_deref(),
                    heading_level,
                    block.is_list,
                    block.has_drawing,
                );

                if has_text || matches!(content_type, ContentType::Image) {
                    let normalized = if has_text {
                        text
                    } else {
                        image_placeholder(block.image_alt.as_deref())
                    };
                    out.push(DocumentElement {
                        content_type,
                        text: normalized,
                        page_number: Some(block_page),
                        heading_level: if matches!(content_type, ContentType::HeadingSection) {
                            heading_level
                        } else {
                            None
                        },
                        footnote_refs: block.footnote_refs.clone(),
                        endnote_refs: block.endnote_refs.clone(),
                        image_rid: block.image_rid.clone(),
                    });

                    // A paragraph can hold a whole gallery, but only its first
                    // blip rides along on the element above — the rest were
                    // dropped. poi_VariousPictures.docx puts five drawings in
                    // one <w:p>, and because the first is an .wmf we cannot
                    // decode, it returned NO images at all. (#13)
                    for (rid, alt) in block.images.iter().skip(1) {
                        out.push(DocumentElement {
                            content_type: ContentType::Image,
                            text: image_placeholder(
                                alt.as_deref().or(block.image_alt.as_deref()),
                            ),
                            page_number: Some(block_page),
                            heading_level: None,
                            footnote_refs: Vec::new(),
                            endnote_refs: Vec::new(),
                            image_rid: Some(rid.clone()),
                        });
                    }
                }
            }
        }

        if triggers_page_break {
            current_page += 1;
        }
    }

    out
}

fn classify_paragraph_content(
    text: &str,
    style_val: Option<&str>,
    heading_level: Option<u32>,
    is_list: bool,
    has_drawing: bool,
) -> ContentType {
    let style_lc = style_val.map(|s| s.to_ascii_lowercase());
    let is_caption = style_lc
        .as_deref()
        .map(|s| s.contains("caption"))
        .unwrap_or(false);
    let is_code = style_lc
        .as_deref()
        .map(|s| s.contains("code"))
        .unwrap_or(false)
        || text.contains("```");

    if heading_level.is_some() {
        ContentType::HeadingSection
    } else if is_caption {
        ContentType::FootnoteCaption
    } else if is_list {
        ContentType::BulletNumberedList
    } else if is_code {
        ContentType::CodeBlock
    } else if has_drawing {
        ContentType::Image
    } else if text.len() > LONG_PARAGRAPH_THRESHOLD {
        ContentType::LongSingleParagraph
    } else if text.len() < SHORT_PARAGRAPH_THRESHOLD {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

fn build_chunks_from_elements(
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

fn build_chunks_from_elements_with_images(
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

fn flush_outside_shorts(
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
        let (chunk_fns, chunk_ens): (&[(String, String)], &[(String, String)]) = if idx == 0 {
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

fn flush_section(
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

        chunks.push(ChunkRecordInput {
            content_type,
            content: combined,
            metadata: base_chunk_metadata(
                heading,
                heading_level,
                &fns,
                &ens,
                doc_metadata,
                section_page,
            ),
        });
    }

    section_parts.clear();
    *section_heading = None;
}

fn base_chunk_metadata(
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
fn collect_element_refs(
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

fn extract_text_from_xml(xml: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut cursor = 0usize;

    while let Some(start) = find_next_wt_tag_start(xml, cursor) {
        let open_end = xml[start..]
            .find('>')
            .map(|i| start + i + 1)
            .ok_or_else(|| "Malformed text tag in DOCX XML".to_string())?;

        if open_end >= 2 && xml[open_end - 2..open_end].starts_with("/>") {
            cursor = open_end;
            continue;
        }

        let close = xml[open_end..]
            .find("</w:t>")
            .map(|i| open_end + i)
            .ok_or_else(|| {
                let start_snippet = start.saturating_sub(30);
                let end_snippet = (open_end + 60).min(xml.len());
                let snippet = &xml[start_snippet..end_snippet];
                format!("Unclosed text tag in DOCX XML near: {}", snippet)
            })?;

        let raw = &xml[open_end..close];
        let decoded = unescape(raw)
            .map_err(|e| format!("Failed to decode XML entities: {e}"))?
            .into_owned();

        if !decoded.trim().is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(decoded.trim());
        }

        cursor = close + "</w:t>".len();
    }

    Ok(out)
}

fn find_next_wt_tag_start(xml: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some(rel_start) = xml[cursor..].find("<w:t") {
        let start = cursor + rel_start;
        let next_index = start + 4;
        let next = xml.as_bytes().get(next_index).copied();

        if matches!(
            next,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            return Some(start);
        }

        cursor = next_index;
    }
    None
}

pub(super) fn extract_notes_map(xml: &str, tag: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let open_marker = format!("<w:{tag}");
    let close_marker = format!("</w:{tag}>");
    let mut cursor = 0usize;

    while let Some(rel_start) = xml[cursor..].find(&open_marker) {
        let start = cursor + rel_start;
        let end = match xml[start..].find(close_marker.as_str()) {
            Some(i) => start + i + close_marker.len(),
            None => break,
        };

        let block = &xml[start..end];

        // Skip the auto-generated separator entries (w:type="separator",
        // "continuationSeparator", "continuationNotice") that Word inserts at
        // the top of footnotes.xml / endnotes.xml — they carry no author
        // content and would only add noise if surfaced to consumers.
        let note_type = first_tag_attr(block, "w:type");
        let is_separator = matches!(
            note_type.as_deref(),
            Some("separator") | Some("continuationSeparator") | Some("continuationNotice")
        );
        if is_separator {
            cursor = end;
            continue;
        }

        if let Some(id) = first_tag_attr(block, "w:id") {
            if let Ok(text) = extract_text_from_xml(block) {
                let clean = text.trim().to_string();
                if !clean.is_empty() {
                    out.insert(id, clean);
                }
            }
        }

        cursor = end;
    }

    out
}

/// Read an attribute value out of the first `<…>` tag in `block`. Returns
/// `None` when the tag has no such attribute. Tolerates both `"` and `'`
/// quoted values, but assumes the opening tag does not contain a stray `>`
/// inside an attribute value (always true for well-formed DOCX XML).
fn first_tag_attr(block: &str, attr: &str) -> Option<String> {
    let tag_end = block.find('>')?;
    let header = &block[..tag_end];
    let needle = format!("{attr}=");
    let mut search_from = 0usize;
    while let Some(rel) = header[search_from..].find(&needle) {
        let idx = search_from + rel;
        // Ensure this match is at a token boundary (preceded by whitespace,
        // `<`, or string start) so we don't match `xyz{attr}=` substrings.
        let prev = idx.checked_sub(1).and_then(|p| header.as_bytes().get(p));
        let boundary_ok = matches!(
            prev,
            None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'/')
        );
        if !boundary_ok {
            search_from = idx + needle.len();
            continue;
        }
        let after = &header[idx + needle.len()..];
        let quote_char = after.chars().next()?;
        if quote_char != '"' && quote_char != '\'' {
            return None;
        }
        let rest = &after[1..];
        let end_q = rest.find(quote_char)?;
        return Some(rest[..end_q].to_string());
    }
    None
}

pub(super) fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> Result<Option<String>, String> {
    match archive.by_name(name) {
        Ok(mut file) => {
            let size = file.size();
            if size > max_bytes {
                return Err(format!(
                    "{name} is too large after decompression: {} bytes (limit: {} bytes)",
                    size, max_bytes
                ));
            }
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| format!("Failed to read {name}: {e}"))?;
            Ok(Some(content))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("Failed to open {name}: {e}")),
    }
}

fn read_first_prefixed_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    prefix: &str,
    max_bytes: u64,
) -> Result<Option<String>, String> {
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to inspect zip entry at index {i}: {e}"))?;
        if file.name().starts_with(prefix) {
            let size = file.size();
            if size > max_bytes {
                return Err(format!(
                    "{} is too large after decompression: {} bytes (limit: {} bytes)",
                    file.name(),
                    size,
                    max_bytes
                ));
            }
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| format!("Failed to read {}: {e}", file.name()))?;
            return Ok(Some(content));
        }
    }
    Ok(None)
}

fn count_prefixed_entries<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    prefix: &str,
) -> Result<usize, String> {
    let mut count = 0usize;
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to inspect zip entry at index {i}: {e}"))?;
        if file.name().starts_with(prefix) {
            count += 1;
        }
    }
    Ok(count)
}

fn semantic_chunks(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.trim().to_string()];
    }

    let sentences = split_sentences(text);
    let mut out = Vec::new();
    let mut current = String::new();

    for s in sentences {
        let candidate = if current.is_empty() {
            s.clone()
        } else {
            format!("{} {}", current, s)
        };

        if candidate.len() <= max_chars {
            current = candidate;
        } else {
            if !current.is_empty() {
                out.push(current.trim().to_string());
            }
            current = s;
        }
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    if out.is_empty() {
        vec![text.trim().to_string()]
    } else {
        out
    }
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
            current.clear();
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }

    out
}

fn recursive_char_chunks(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }

    let split_at = crate::shared::floor_char_boundary(text, max_chars.min(text.len()));
    let head = text[..split_at].trim().to_string();
    let tail_start = crate::shared::floor_char_boundary(text, split_at.saturating_sub(overlap));
    let tail = text[tail_start..].trim().to_string();

    let mut out = vec![head];
    if !tail.is_empty() && tail.len() < text.len() {
        out.extend(recursive_char_chunks(&tail, max_chars, overlap));
    }
    out
}


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

pub(super) fn chunk_with_images(bytes: &[u8]) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let parsed = parse_docx_document(bytes)?;
    let mut image_out = Vec::new();
    let combined = build_chunks_from_elements_with_images(
        parsed.elements, &parsed.doc_metadata, &parsed.footnote_map, &parsed.endnote_map,
        &image_rids_map, &mut archive, &mut image_out,
    );
    Ok((
        combined
            .into_iter()
            .map(|c| crate::chunk::Chunk::new(c.content, c.content_type.as_str(), c.metadata))
            .collect(),
        image_out,
    ))
}
