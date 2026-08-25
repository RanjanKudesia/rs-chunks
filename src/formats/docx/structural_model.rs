//! Constants, element model and document parsing for the DOCX
//! structural/default mode.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Cursor;
use zip::ZipArchive;

use super::common::{
    docx_heading_level, image_placeholder, parse_docx_blocks, DocxBlock, DocxBlockKind,
};
use super::docx_aux::{
    count_prefixed_entries, extract_notes_map, extract_text_from_xml, read_first_prefixed_entry,
    read_zip_entry,
};

pub(super) const MAX_DOCX_AUX_XML_BYTES: u64 = 10 * 1024 * 1024;

/// A paragraph whose visible text length (in bytes) is at or above this
/// threshold is classified as `LongSingleParagraph` and will be split with
/// `semantic_chunks` before emission.
pub(super) const LONG_PARAGRAPH_THRESHOLD: usize = 500;

/// A paragraph whose visible text length (in characters) is strictly below this
/// threshold is classified as `ShortDisconnectedParagraph` and aggregated
/// with neighbouring shorts before emission.
pub(super) const SHORT_PARAGRAPH_THRESHOLD: usize = 80;

/// Whether a paragraph reads as a finished thought rather than a stray
/// fragment.
///
/// The short-paragraph bucket exists to glue captions, labels and stray lines
/// back together. Length alone cannot tell those from a complete sentence in a
/// dense script: `这是一个中文段落，用于测试提取和再生成。` is a whole sentence in
/// 20 characters, and measuring bytes instead only moves the cliff rather than
/// removing it. Ending on sentence-final punctuation is the script-neutral
/// signal — `Figure 3` and `Screen Reader` do not, in any language.
/// Cap on an assembled heading section (TECH_DEBT #91).
///
/// Matches `MAX_CHUNK_CHARS` in `txt`, `html` and `md`, so a docx section is
/// bounded the same way every other prose format is. Named separately from
/// `docx::section::MAX_SECTION_CHARS`, which caps a different thing in a
/// different mode — one constant serving two bounds would imply they must move
/// together, and they need not.
pub(super) const MAX_SECTION_CHARS: usize = 1200;

/// Pathological-only bound on a single rendered Table element.
///
/// A blast-radius limiter, not a prose cap — deliberately 5x
/// `MAX_SECTION_CHARS`. The largest table in an ordinary corpus document is
/// 1,737 chars (`poi_drawing.docx`); the only fixtures above this line are
/// `poi_bug65649.docx` (522,261), `poi_bug59058.docx` (93,066) and
/// `_stress_big_table.docx` (39,673). The measurement gap between 3,552 and
/// 39,673 is empty, so nothing ordinary is near it.
pub(super) const MAX_TABLE_CHARS: usize = MAX_SECTION_CHARS * 5;

/// The markdown header block a split table repeats onto every part: the header
/// row PLUS its `| --- |` separator, hence 2 and not 1. With 1 the
/// continuation parts get a header and no separator, which is not a valid
/// markdown table. Matches `md/common.rs`'s Table handling.
pub(super) const TABLE_HEADER_LINES: usize = 2;

fn is_complete_sentence(text: &str) -> bool {
    const SENTENCE_END: [char; 8] = [
        '.', '!', '?', '\u{3002}', '\u{ff01}', '\u{ff1f}', '\u{61f}', '\u{5c3}',
    ];
    let t = text.trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}', ' ']);
    t.chars().count() >= 12 && t.ends_with(SENTENCE_END)
}

/// Maximum target size (in bytes) for one semantic sub-chunk when splitting
/// a long paragraph or image-caption block.
pub(super) const SEMANTIC_SPLIT_MAX_BYTES: usize = 900;

/// Target size (in bytes) for one chunk when aggregating short disconnected
/// paragraphs via `recursive_char_chunks`.
pub(super) const SHORT_AGGREGATE_CHUNK_SIZE: usize = 700;

/// Character overlap between successive `recursive_char_chunks` outputs when
/// aggregating short disconnected paragraphs.
pub(super) const SHORT_AGGREGATE_CHUNK_OVERLAP: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContentType {
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
pub(super) struct DocumentElement {
    pub(super) content_type: ContentType,
    pub(super) text: String,
    pub(super) page_number: Option<usize>,
    /// Resolved heading level (1-based, where 1 is the highest) for elements
    /// classified as `HeadingSection`. Computed by
    /// [`docx_heading_level`] from the source paragraph's `<w:pStyle>` and
    /// `<w:outlineLvl>` so the same English / French / German style names
    /// and outline fallback used by the other DOCX chunkers apply here too.
    /// `None` for non-heading elements.
    pub(super) heading_level: Option<u32>,
    /// IDs of footnotes (`word/footnotes.xml`) referenced by this element's
    /// source paragraph, in document order. Empty for elements derived from
    /// tables or paragraphs without any `<w:footnoteReference>`.
    pub(super) footnote_refs: Vec<String>,
    /// IDs of endnotes (`word/endnotes.xml`) referenced by this element's
    /// source paragraph.
    pub(super) endnote_refs: Vec<String>,
    /// Relationship ID (`r:embed`) of the image contained in this element.
    /// `None` for non-image elements or when the rId was not captured.
    pub(super) image_rid: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct DocParseResult {
    pub(super) elements: Vec<DocumentElement>,
    pub(super) doc_metadata: Value,
    /// `w:id` → resolved footnote text, excluding separator / continuation
    /// auto-entries.
    pub(super) footnote_map: HashMap<String, String>,
    /// `w:id` → resolved endnote text.
    pub(super) endnote_map: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub(super) struct ChunkRecordInput {
    pub(super) content_type: ContentType,
    pub(super) content: String,
    pub(super) metadata: Value,
}

impl ContentType {
    pub(super) fn as_str(self) -> &'static str {
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

pub(super) fn parse_docx_document(bytes: &[u8]) -> Result<DocParseResult, String> {
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
                // Pictures inside table cells are content like any other. (#71)
                for (rid, alt) in &block.images {
                    out.push(DocumentElement {
                        content_type: ContentType::Image,
                        text: image_placeholder(alt.as_deref()),
                        page_number: Some(block_page),
                        heading_level: None,
                        footnote_refs: Vec::new(),
                        endnote_refs: Vec::new(),
                        image_rid: Some(rid.clone()),
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
                    let normalized = super::common::text_with_image_marker(
                        text,
                        block.has_drawing,
                        block.image_alt.as_deref(),
                    );
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
                            text: image_placeholder(alt.as_deref().or(block.image_alt.as_deref())),
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
    } else if text.len() < SHORT_PARAGRAPH_THRESHOLD && !is_complete_sentence(text) {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}
