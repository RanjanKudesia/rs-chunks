//! Hub for the shared DOCX helpers used by every DOCX mode.
//!
//! The implementations live in focused sibling modules; this module keeps the
//! extension surface plus `pub(super)` re-exports so every consumer keeps
//! importing from `super::common` unchanged:
//!
//! - `xml_text` — XML/text primitives (`qname_eq`, `push_text`, …)
//! - `block_model` — the block/paragraph event model
//! - `block_api` — the `parse_docx_*` entry points every mode calls
//! - `table_render` — table → Markdown pipe-table rendering
//! - `harvest` — alt-text / note-id / blip attribute harvesting
//! - `images_rels` — image placeholders, rels parsing, image chunks
//! - `stream_walker` — the canonical streaming `document.xml` walker

/// Every WordprocessingML OOXML extension routed through the docx chunker. They
/// all store the body in `word/document.xml`, which the parser reads by part name
/// regardless of extension, so the same code path serves every variant.
/// Adding a future Word variant is a one-line change here.
pub const WORD_OOXML_EXTS: &[&str] = &[".docx", ".docm", ".dotx", ".dotm"];

/// True if `path` ends in any supported Word OOXML extension (case-insensitive).
pub fn is_word_ooxml(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    WORD_OOXML_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Human-readable extension list for error messages.
pub fn word_exts_display() -> String {
    WORD_OOXML_EXTS.join(", ")
}

#[allow(unused_imports)]
pub(super) use super::block_api::{
    parse_docx_blocks, parse_docx_indexed_items_with_images, parse_docx_indexed_paragraphs,
    parse_docx_paragraph_events, parse_docx_paragraph_events_with_images, ParaOrImage,
};
#[allow(unused_imports)]
pub(super) use super::block_model::{
    docx_heading_level, DocxBlock, DocxBlockKind, IndexedParagraph, PageBreakSignal,
    ParagraphEvent, MIN_PARAGRAPH_CHARS,
};
#[allow(unused_imports)]
pub(super) use super::images_rels::{
    collect_image_chunks_from_items, image_hash_name, image_placeholder,
    open_docx_archive_with_rids, parse_rels_xml_images, text_with_image_marker,
};
#[allow(unused_imports)]
pub(super) use super::xml_text::{collapse_whitespace, push_event_text, push_text, qname_eq};
