//! Block model and small text/diagram/chart helpers for PPTX markdown.

use std::io::Cursor;

use zip::ZipArchive;

use super::common::read_zip_entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockKind {
    Paragraph,
    ListItem,
    Table,
    Image,
}

#[derive(Debug, Clone)]
pub(super) struct SlideBlock {
    pub(super) kind: BlockKind,
    /// Plain or inline-formatted text (for Paragraph, ListItem, Image).
    pub(super) text: String,
    /// Indent level 0-based (for ListItem).
    pub(super) level: u8,
    /// True = ordered/numbered list, false = bullet (for ListItem).
    pub(super) is_numbered: bool,
    /// Table rows: outer = rows, inner = cells (for Table).
    pub(super) table_rows: Vec<Vec<String>>,
    /// Whether to render the first row as a header separator (for Table).
    pub(super) table_has_header: bool,
    /// Relationship id from `<a:blip r:embed="rIdN"/>` for image blocks.
    pub(super) image_rid: Option<String>,
}

impl SlideBlock {
    pub(super) fn paragraph(text: String) -> Self {
        Self {
            kind: BlockKind::Paragraph,
            text,
            level: 0,
            is_numbered: false,
            table_rows: Vec::new(),
            table_has_header: false,
            image_rid: None,
        }
    }

    pub(super) fn list_item(text: String, level: u8, is_numbered: bool) -> Self {
        Self {
            kind: BlockKind::ListItem,
            text,
            level,
            is_numbered,
            table_rows: Vec::new(),
            table_has_header: false,
            image_rid: None,
        }
    }

    pub(super) fn table(rows: Vec<Vec<String>>, has_header: bool) -> Self {
        Self {
            kind: BlockKind::Table,
            text: String::new(),
            level: 0,
            is_numbered: false,
            table_rows: rows,
            table_has_header: has_header,
            image_rid: None,
        }
    }

    pub(super) fn image(alt: Option<String>, rid: Option<String>) -> Self {
        let text = match alt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(a) => format!("[Image: {a}]"),
            None => "[Image]".to_string(),
        };
        Self {
            kind: BlockKind::Image,
            text,
            level: 0,
            is_numbered: false,
            table_rows: Vec::new(),
            table_has_header: false,
            image_rid: rid,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct SlideMarkdownContent {
    pub title: Option<String>,
    pub blocks: Vec<SlideBlock>,
    pub notes: Option<String>,
    /// `r:dm` ids of SmartArt diagrams; their text lives in `ppt/diagrams/`.
    pub diagram_rids: Vec<String>,
    /// `r:id` ids of charts; their data lives in `ppt/charts/`.
    pub chart_rids: Vec<String>,
}

/// Append a slide's SmartArt text as paragraph blocks. Shared by both markdown
/// entry points so `get_markdown` and `get_markdown_with_images` agree.
pub(super) fn append_diagram_blocks(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
    slide: &mut SlideMarkdownContent,
) {
    let parts = super::diagram::resolve_diagram_parts(archive, slide_name, &slide.diagram_rids);
    for part in parts {
        if let Ok(bytes) = read_zip_entry(archive, &part) {
            for text in super::diagram::parse_diagram_xml(&bytes) {
                slide.blocks.push(SlideBlock::paragraph(text));
            }
        }
    }
}

/// Append a slide's chart data as a real markdown table. The block renderer
/// already knows how to draw one, so a chart costs no new rendering code.
pub(super) fn append_chart_blocks(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
    slide: &mut SlideMarkdownContent,
) {
    let parts = super::chart::resolve_chart_parts(archive, slide_name, &slide.chart_rids);
    for part in parts {
        if let Ok(bytes) = read_zip_entry(archive, &part) {
            let rows = super::chart::parse_chart_xml(&bytes);
            if !rows.is_empty() {
                slide
                    .blocks
                    .push(SlideBlock::paragraph("Chart".to_string()));
                slide.blocks.push(SlideBlock::table(rows, true));
            }
        }
    }
}

pub(super) fn push_text(dst: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !dst.is_empty() {
        dst.push(' ');
    }
    dst.push_str(trimmed);
}

/// Append text that came from an entity reference, with no separator.
///
/// A reference splits one element's text into several events: `AT&amp;T`
/// arrives as `"AT"`, `"&"`, `"T"`. [`push_text`] space-joins successive events,
/// which is right when each event *was* a whole element and wrong here — it
/// produced `AT & T` in `get_markdown` while `get_chunks` (which does not use
/// this walker) correctly produced `AT&T` (TECH_DEBT L6).
///
/// The text is appended verbatim: no trim, no separator. Trimming would eat the
/// spacing of an entity like `&nbsp;`, and a separator is the bug itself.
pub(super) fn push_entity_text(dst: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    dst.push_str(text);
}

pub(super) fn attr_local_name(key: &[u8]) -> &[u8] {
    key.rsplit(|b| *b == b':').next().unwrap_or(key)
}

/// Resolve XML entities in an attribute value.
///
/// Delegates to the shared resolver so an attribute gets the same entity table
/// element text does — quick-xml's own `unescape_value` knows only the five
/// predefined names, and errored (falling back to the raw, still-escaped bytes)
/// on anything else.
pub(super) fn decode_attr(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    crate::entities::decode_attr(attr)
}

/// Squash any run of whitespace (including the newlines `&#xA;` decodes to)
/// into single spaces, so the text can be rendered on one line.
pub(super) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
