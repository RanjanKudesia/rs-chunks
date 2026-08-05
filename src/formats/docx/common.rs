//! Shared XML helpers and a unified DOCX paragraph parser used by every
//! DOCX mode.
//!
//! Step A.1 of the parser unification moved `qname_eq`, `push_text` and
//! `collapse_whitespace` here. Step A.2 adds a single
//! `parse_docx_indexed_paragraphs` function that replaces the previously
//! duplicated walkers in `sliding_window.rs` and `sentence.rs`.

use crate::entities::read_event_folding_entities;
use std::collections::HashMap;
use std::io::{BufReader, Cursor, Read};

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader;
use zip::ZipArchive;

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

/// True when `name` matches `expected`, ignoring any XML namespace prefix
/// (e.g. matches `w:p` against `b"p"` and a bare `p` against `b"p"`).
pub(super) fn qname_eq(name: QName<'_>, expected: &[u8]) -> bool {
    let n = name.as_ref();
    n == expected || n.rsplit(|b| *b == b':').next() == Some(expected)
}

/// Append `piece` to `target`, trimming whitespace and inserting a single
/// space separator when both sides are non-empty. Used to stitch the run
/// fragments inside a paragraph back into a single string.
pub(super) fn push_text(target: &mut String, piece: &str) {
    let trimmed = piece.trim();
    if trimmed.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(trimmed);
}

/// Append text that came from an entity reference.
///
/// [`push_text`] space-joins successive pieces, which is right when each piece
/// is a whole element's text. An entity reference splits one element into
/// several events, so joining there inserts a space in the middle of a word:
/// `AT&amp;T` becomes `AT & T`. Entity text is appended exactly as-is instead.
pub(super) fn push_event_text(target: &mut String, piece: &str, is_entity: bool) {
    if is_entity {
        target.push_str(piece);
    } else {
        push_text(target, piece);
    }
}

/// Collapse runs of whitespace (including newlines/tabs) into single spaces
/// and trim the result. Mirrors the previous per-file implementations.
pub(super) fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }

    out.trim().to_string()
}

// ─── Unified DOCX paragraph parser ──────────────────────────────────────────

/// Minimum paragraph length (in bytes) for flat-paragraph consumers. Mirrors
/// the previous per-file constant.
pub(super) const MIN_PARAGRAPH_CHARS: usize = 10;

/// Per-paragraph page-break signal produced by [`parse_docx_paragraph_events`].
/// `Explicit` indicates a `<w:br w:type="page"/>`, `Section` a `<w:sectPr>`
/// boundary, `Rendered` a `<w:lastRenderedPageBreak/>` hint left by Word
/// after the last render, and `None` no in-document boundary marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageBreakSignal {
    Explicit,
    Section,
    Rendered,
    None,
}

/// A single logical paragraph plus its trailing boundary signal. Tables are
/// emitted as a single event with `signal == PageBreakSignal::None` (the
/// caller treats them like any other paragraph).
#[derive(Debug, Clone)]
pub(super) struct ParagraphEvent {
    pub text: String,
    pub signal: PageBreakSignal,
    pub is_heading: bool,
    pub heading_level: Option<u32>,
    pub is_list: bool,
    pub is_table: bool,
}

/// A single logical paragraph emitted by [`parse_docx_indexed_paragraphs`].
/// `index` reflects the position in the document's stream of accepted
/// paragraphs (i.e. it skips paragraphs below `MIN_PARAGRAPH_CHARS`).
#[derive(Debug, Clone)]
pub(super) struct IndexedParagraph {
    pub index: usize,
    pub text: String,
    pub is_heading: bool,
    pub heading_level: Option<u32>,
    pub is_list: bool,
    pub is_table: bool,
}

/// Resolve a DOCX heading level from `<w:pStyle val="..."/>` and/or
/// `<w:outlineLvl val="..."/>`. Returns `Some(level)` (1-based, where 1 is
/// the highest) when either signal identifies the paragraph as a heading.
///
/// Style detection: case-insensitive match against names starting with
/// `heading` (English `Heading1`..`Heading9`) plus a few common Word
/// localisations (`title` → level 1, `Titre1` French, `Überschrift1`
/// German). If a trailing 1-digit number is present it is used as the level,
/// otherwise level defaults to 1.
///
/// Outline fallback: when no style match is found but `<w:outlineLvl>`
/// resolved to `n`, level is `n + 1` (Word stores outline levels 0-based).
pub(super) fn docx_heading_level(style: Option<&str>, outline: Option<u32>) -> Option<u32> {
    if let Some(raw) = style {
        let lower = raw.to_ascii_lowercase();
        let prefixes: &[(&str, u32)] = &[
            ("heading", 1),
            ("titre", 1),
            ("überschrift", 1),
            ("title", 1),
        ];
        for (prefix, default_level) in prefixes {
            if let Some(rest) = lower.strip_prefix(prefix) {
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<u32>() {
                    if n > 0 {
                        return Some(n);
                    }
                }
                return Some(*default_level);
            }
        }
    }
    outline.map(|n| n + 1)
}

/// Kind of block emitted by [`parse_docx_blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DocxBlockKind {
    Paragraph,
    Table,
}

/// A raw block emitted by the canonical DOCX walker. Every consumer-specific
/// normalization (whitespace collapsing, length filtering, image placeholder,
/// list-item prefixing, heading detection, …) is applied by callers on top of
/// this stream.
///
/// - For `Paragraph` kind: `text` is the concatenation of run text fragments
///   (joined with a single space by [`push_text`]), no further normalization
///   applied.
/// - For `Table` kind: `text` is the table rendered as a Markdown pipe table
///   (`| a | b |` rows with a `| --- |` separator after the header row).
///   Header rows are detected via `<w:tblHeader/>` in `<w:trPr>`; if absent,
///   the first row of a multi-row table is treated as the header. Empty
///   cells are preserved so columns stay aligned, rows are padded to the
///   maximum column count, and pipe / newline characters inside cells are
///   escaped. Nested tables are flattened inline into the parent cell
///   (`row1cells | row1cells; row2cells | row2cells`).
#[derive(Debug, Clone)]
pub(super) struct DocxBlock {
    pub kind: DocxBlockKind,
    pub text: String,
    pub has_drawing: bool,
    pub is_list: bool,
    pub list_level: u8,
    pub heading_style: Option<String>,
    pub outline_level: Option<u32>,
    pub page_break: bool,
    pub section_break: bool,
    pub rendered_page_break: bool,
    /// Author-provided alt text for inline images, in priority order:
    /// `wp:docPr/@descr`, `wp:docPr/@title`, `pic:cNvPr/@descr`,
    /// `pic:cNvPr/@name`. `None` when the paragraph contains no
    /// `<w:drawing>` or the drawing exposes no usable description.
    pub image_alt: Option<String>,
    /// Relationship id from `<a:blip r:embed="rIdN"/>` for inline images.
    pub image_rid: Option<String>,
    /// Every `<a:blip r:embed>` in the paragraph, in document order, each with
    /// the alt text of its own drawing.
    ///
    /// `image_rid`/`image_alt` above hold only the FIRST, which silently
    /// dropped the rest: a paragraph whose first drawing is an unsupported
    /// `.wmf`/`.emf` returned no image at all, because the supported `.png`
    /// beside it was never even captured (#13).
    pub images: Vec<(String, Option<String>)>,
    /// IDs of footnotes referenced by `<w:footnoteReference>` inside this
    /// paragraph, in document order. Resolved against `word/footnotes.xml`
    /// at the structural / chunking layer so anchored footnote text lands
    /// on the same chunk as its referring paragraph instead of being
    /// dumped at end-of-document.
    pub footnote_refs: Vec<String>,
    /// Same as `footnote_refs` but for `<w:endnoteReference>` →
    /// `word/endnotes.xml`.
    pub endnote_refs: Vec<String>,
    /// `<w:numId w:val="N"/>` from `<w:numPr>`. Used by `to_markdown` to look
    /// up the list format (ordered vs bullet) in `word/numbering.xml`.
    /// `None` for non-list paragraphs and table blocks.
    pub num_id: Option<u32>,
    /// Hyperlinks in this paragraph: `(anchor_text, r:id)` pairs in document
    /// order. The `r:id` value is resolved to a URL against
    /// `word/_rels/document.xml.rels` by the markdown serialiser.
    /// Empty for table blocks and paragraphs with no `<w:hyperlink>`.
    pub hyperlinks: Vec<(String, String)>,
}

/// Parse a `.docx` byte slice into a flat list of paragraphs, flattening
/// tables to a Markdown pipe-table representation and replacing image-only
/// paragraphs with `"[Image]"` (or `"[Image: <alt>]"` when alt text is
/// available). Thin wrapper around
/// [`parse_docx_paragraph_events`] that drops boundary signals and assigns
/// sequential indices.
pub(super) fn parse_docx_indexed_paragraphs(bytes: &[u8]) -> Result<Vec<IndexedParagraph>, String> {
    let events = parse_docx_paragraph_events(bytes)?;
    Ok(events
        .into_iter()
        .enumerate()
        .map(|(index, ev)| IndexedParagraph {
            index,
            text: ev.text,
            is_heading: ev.is_heading,
            heading_level: ev.heading_level,
            is_list: ev.is_list,
            is_table: ev.is_table,
        })
        .collect())
}

/// Whitespace-collapsing, length-filtering flavour of the DOCX walker used by
/// `sliding_window`, `sentence` and `page_aware`. Emits one event per
/// accepted paragraph or table along with any `<w:br type="page">` /
/// `<w:sectPr>` boundary signal observed inside the paragraph.
pub(super) fn parse_docx_paragraph_events(bytes: &[u8]) -> Result<Vec<ParagraphEvent>, String> {
    let blocks = parse_docx_blocks(bytes)?;
    let mut events: Vec<ParagraphEvent> = Vec::with_capacity(blocks.len());

    for block in blocks {
        let heading_level = match block.kind {
            DocxBlockKind::Paragraph => {
                docx_heading_level(block.heading_style.as_deref(), block.outline_level)
            }
            DocxBlockKind::Table => None,
        };
        let is_heading = heading_level.is_some();
        let is_list = matches!(block.kind, DocxBlockKind::Paragraph) && block.is_list;
        let is_table = matches!(block.kind, DocxBlockKind::Table);

        let (text, signal) = match block.kind {
            DocxBlockKind::Paragraph => {
                let collapsed = collapse_whitespace(&block.text);
                let normalized = if !collapsed.is_empty() {
                    collapsed
                } else if block.has_drawing {
                    image_placeholder(block.image_alt.as_deref())
                } else {
                    String::new()
                };

                let signal = if block.page_break {
                    PageBreakSignal::Explicit
                } else if block.section_break {
                    PageBreakSignal::Section
                } else if block.rendered_page_break {
                    PageBreakSignal::Rendered
                } else {
                    PageBreakSignal::None
                };
                (normalized, signal)
            }
            DocxBlockKind::Table => {
                let collapsed = collapse_whitespace(&block.text).trim().to_string();
                (collapsed, PageBreakSignal::None)
            }
        };

        if text.len() >= MIN_PARAGRAPH_CHARS {
            events.push(ParagraphEvent {
                text,
                signal,
                is_heading,
                heading_level,
                is_list,
                is_table,
            });
        } else if !matches!(signal, PageBreakSignal::None) {
            // Paragraph is too short to emit, but it carries a page-break
            // signal that downstream consumers (page_aware) must not lose.
            // Promote the signal onto the most recent emitted event so it
            // still triggers a boundary at the right document position.
            if let Some(last) = events.last_mut() {
                if matches!(last.signal, PageBreakSignal::None) {
                    last.signal = signal;
                }
            }
        }
    }

    Ok(events)
}

/// Mixed stream item returned by [`parse_docx_paragraph_events_with_images`].
/// Text paragraphs are wrapped in `Para`; image blocks become `Image`
/// regardless of their alt-text length (the length filter applies only to text).
#[derive(Debug, Clone)]
pub(super) enum ParaOrImage {
    Para(ParagraphEvent),
    Image {
        rid: Option<String>,
        alt: Option<String>,
        signal: PageBreakSignal,
    },
}

/// Image-aware variant of [`parse_docx_paragraph_events`].
///
/// Text paragraphs: identical filtering/normalization as the original
/// (>= MIN_PARAGRAPH_CHARS). Page-break signal promotion for short text
/// paragraphs also works the same.
///
/// Image blocks: always emitted as `ParaOrImage::Image` regardless of
/// alt-text length. The signal is captured in the Image variant so that
/// page_aware mode can detect page breaks on image-carrying paragraphs.
pub(super) fn parse_docx_paragraph_events_with_images(
    bytes: &[u8],
) -> Result<Vec<ParaOrImage>, String> {
    let blocks = parse_docx_blocks(bytes)?;
    let mut items: Vec<ParaOrImage> = Vec::with_capacity(blocks.len());

    for block in blocks {
        let heading_level = match block.kind {
            DocxBlockKind::Paragraph => {
                docx_heading_level(block.heading_style.as_deref(), block.outline_level)
            }
            DocxBlockKind::Table => None,
        };
        let is_heading = heading_level.is_some();
        let is_list = matches!(block.kind, DocxBlockKind::Paragraph) && block.is_list;

        let signal = if block.page_break {
            PageBreakSignal::Explicit
        } else if block.section_break {
            PageBreakSignal::Section
        } else if block.rendered_page_break {
            PageBreakSignal::Rendered
        } else {
            PageBreakSignal::None
        };

        match block.kind {
            DocxBlockKind::Paragraph => {
                let collapsed = collapse_whitespace(&block.text);
                let normalized = if !collapsed.is_empty() {
                    collapsed
                } else if block.has_drawing {
                    image_placeholder(block.image_alt.as_deref())
                } else {
                    String::new()
                };

                if normalized.len() >= MIN_PARAGRAPH_CHARS {
                    items.push(ParaOrImage::Para(ParagraphEvent {
                        text: normalized,
                        signal,
                        is_heading,
                        heading_level,
                        is_list,
                        is_table: false,
                    }));
                } else if !matches!(signal, PageBreakSignal::None) {
                    if let Some(ParaOrImage::Para(last)) = items.last_mut() {
                        if matches!(last.signal, PageBreakSignal::None) {
                            last.signal = signal;
                        }
                    }
                }

                if block.has_drawing {
                    if block.images.is_empty() {
                        // A drawing with no resolvable blip (chart, shape, OLE
                        // object) still counts as one image slot.
                        items.push(ParaOrImage::Image {
                            rid: block.image_rid,
                            alt: block.image_alt,
                            signal,
                        });
                    } else {
                        // One item per blip, so a gallery paragraph yields every
                        // image rather than only its first. (#13)
                        for (rid, alt) in block.images {
                            items.push(ParaOrImage::Image {
                                rid: Some(rid),
                                alt: alt.or_else(|| block.image_alt.clone()),
                                signal,
                            });
                        }
                    }
                }
            }
            DocxBlockKind::Table => {
                let collapsed = collapse_whitespace(&block.text).trim().to_string();
                if collapsed.len() >= MIN_PARAGRAPH_CHARS {
                    items.push(ParaOrImage::Para(ParagraphEvent {
                        text: collapsed,
                        signal: PageBreakSignal::None,
                        is_heading: false,
                        heading_level: None,
                        is_list: false,
                        is_table: true,
                    }));
                }
                // Pictures inside table cells are content like any other. (#71)
                for (rid, alt) in block.images {
                    items.push(ParaOrImage::Image {
                        rid: Some(rid),
                        alt,
                        signal: PageBreakSignal::None,
                    });
                }
            }
        }
    }

    Ok(items)
}

/// Image-aware variant of [`parse_docx_indexed_paragraphs`].
/// Returns text paragraphs as `Para(ParagraphEvent)` and image blocks
/// as `Image { rid, alt }`. Callers assign indices to Para items only.
pub(super) fn parse_docx_indexed_items_with_images(
    bytes: &[u8],
) -> Result<Vec<ParaOrImage>, String> {
    parse_docx_paragraph_events_with_images(bytes)
}

/// Canonical walker for the body of a DOCX document. Emits one [`DocxBlock`]
/// per `<w:p>` or `<w:tbl>` with the raw text plus every signal the
/// consumers care about (drawings, list markers, heading style, outline
/// level, page/section breaks). No filtering or whitespace normalisation is
/// applied — callers decide what to do.
pub(super) fn parse_docx_blocks(bytes: &[u8]) -> Result<Vec<DocxBlock>, String> {
    let cursor = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| format!("DOCX is not a valid zip archive: {e}"))?;

    let mut document_xml_file = archive
        .by_name("word/document.xml")
        .map_err(|_| "word/document.xml not found in DOCX".to_string())?;

    parse_document_xml_blocks_streaming(&mut document_xml_file)
}

/// Per-table accumulator used by [`parse_document_xml_blocks_streaming`].
/// A stack of these supports nested tables.
#[derive(Default)]
struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_cell: bool,
    in_tr_pr: bool,
    in_header_row: bool,
    /// Column span of the current cell (`<w:gridSpan w:val="N"/>`). Defaults
    /// to 1 (no span). When > 1 the cell text is repeated into each spanned
    /// column position so the rendered markdown stays correctly aligned and
    /// every column retains its group-header context.
    cell_span: usize,
    /// One flag per completed row indicating whether `<w:tblHeader/>` was
    /// present in its `<w:trPr>`.
    header_row_flags: Vec<bool>,
    /// Cell content from the most recently completed row, indexed by column.
    /// Used to repeat content in vertically-merged continuation cells.
    vmerge_col_content: Vec<String>,
    /// Whether the current cell is a vMerge continuation (no "restart").
    cur_cell_is_vmerge_continuation: bool,
}

fn escape_md_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

fn render_table_markdown(state: &TableState) -> String {
    if state.rows.is_empty() {
        return String::new();
    }
    let max_cols = state.rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    // Number of leading rows explicitly marked as header. Fallback: treat
    // first row as header when the table has more than one row.
    let mut header_count = state.header_row_flags.iter().take_while(|f| **f).count();
    if header_count == 0 && state.rows.len() > 1 {
        header_count = 1;
    }

    let mut out = String::new();
    for (i, row) in state.rows.iter().enumerate() {
        out.push('|');
        for col in 0..max_cols {
            let cell = row.get(col).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&escape_md_cell(cell));
            out.push_str(" |");
        }
        out.push('\n');
        if i + 1 == header_count && header_count < state.rows.len() {
            out.push('|');
            for _ in 0..max_cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn render_table_inline(state: &TableState) -> String {
    let mut rows: Vec<String> = Vec::with_capacity(state.rows.len());
    for row in &state.rows {
        let cells: Vec<String> = row.iter().map(|c| c.replace(['\n', '\r'], " ")).collect();
        rows.push(cells.join(" | "));
    }
    rows.join("; ")
}

/// Pull author-provided alt text from a `<wp:docPr>` or `<pic:cNvPr>` start /
/// empty event. Attributes are read in priority order so the most descriptive
/// value wins: `descr` (the "Alt text — Description" field in Word) →
/// `title` (the "Alt text — Title" field) → `name` (the auto-generated image
/// name like `"Picture 3"`, only useful as a last-resort fallback).
/// Returns `None` when no non-empty attribute is found.
fn harvest_image_alt(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let mut descr: Option<String> = None;
    let mut title: Option<String> = None;
    let mut name: Option<String> = None;
    for attr in e.attributes().flatten() {
        let value = String::from_utf8_lossy(attr.value.as_ref())
            .trim()
            .to_string();
        if value.is_empty() {
            continue;
        }
        if qname_eq(attr.key, b"descr") {
            descr.get_or_insert(value);
        } else if qname_eq(attr.key, b"title") {
            title.get_or_insert(value);
        } else if qname_eq(attr.key, b"name") {
            // Skip auto-generated names like "Picture 1" / "Image 3" —
            // they add noise without semantic value.
            let lower = value.to_ascii_lowercase();
            let looks_generic = lower.starts_with("picture ")
                || lower.starts_with("image ")
                || lower.starts_with("graphic ")
                || lower.starts_with("chart ");
            if !looks_generic {
                name.get_or_insert(value);
            }
        }
    }
    descr.or(title).or(name)
}

/// Pull the `w:id` attribute from a `<w:footnoteReference>` or
/// `<w:endnoteReference>` event. The id is the document-order link to an
/// entry in `word/footnotes.xml` / `word/endnotes.xml`. Returns `None` when
/// the attribute is missing or empty.
fn harvest_note_id(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        if qname_eq(attr.key, b"id") {
            let value = String::from_utf8_lossy(attr.value.as_ref())
                .trim()
                .to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Pull the `r:embed` value from `<a:blip>`.
fn harvest_blip_embed(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        if qname_eq(attr.key, b"embed") {
            let value = String::from_utf8_lossy(attr.value.as_ref())
                .trim()
                .to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Render the placeholder string used in place of an image-only paragraph's
/// body. Includes the harvested alt text when available so downstream
/// consumers (embedders, search) get a real semantic signal instead of an
/// opaque marker.
pub(super) fn image_placeholder(alt: Option<&str>) -> String {
    match alt.map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => format!("[Image: {a}]"),
        None => "[Image]".to_string(),
    }
}

/// A paragraph's chunk text, keeping **both** its words and its image marker.
///
/// `get_markdown` emits the text and then the placeholder ([#2](TECH_DEBT.md)).
/// `get_chunks` did the opposite of what markdown used to do — it kept the text
/// and dropped the marker — so a paragraph holding both lost its alt text from
/// the chunks entirely, while a paragraph holding *only* an image did get
/// `[Image: …]`. Chunks disagreed with markdown and with themselves
/// ([#83](TECH_DEBT.md)).
pub(super) fn text_with_image_marker(text: String, has_drawing: bool, alt: Option<&str>) -> String {
    if !has_drawing {
        return text;
    }
    let placeholder = image_placeholder(alt);
    if text.trim().is_empty() {
        return placeholder;
    }
    format!("{text}\n{placeholder}")
}

/// Parse `word/_rels/document.xml.rels` and return a map of `rId → zip path`
/// for image relationships (Type ending in `/image`).
pub(super) fn parse_rels_xml_images(xml: &str) -> HashMap<String, String> {
    let mut images = HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"Relationship" {
                    let mut id = String::new();
                    let mut target = String::new();
                    let mut rel_type = String::new();

                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                        match key.as_str() {
                            "Id" => id = val,
                            "Target" => target = val,
                            "Type" => rel_type = val,
                            _ => {}
                        }
                    }

                    if rel_type.ends_with("/image") && !id.is_empty() && !target.is_empty() {
                        let normalized = if let Some(stripped) = target.strip_prefix("../") {
                            format!("word/{stripped}")
                        } else if target.starts_with("word/") {
                            target
                        } else {
                            format!("word/{target}")
                        };
                        images.insert(id, normalized);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    images
}

/// Hash image bytes and return `"<16hex>.<ext>"` or `None` for unsupported
/// formats (`.emf`, `.wmf`, etc.).
pub(super) fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    let path = zip_path.to_ascii_lowercase();
    crate::image_naming::name_for_path(bytes, &path)
}

/// Returns true when the paragraph style name indicates a list item without
/// requiring `<w:numPr>`. Matches Word's built-in list styles (ListNumber,
/// ListBullet, List, and their numbered variants like ListNumber2) as well as
/// the common single-word aliases used by older or third-party templates.
fn is_list_style(style: &str) -> bool {
    let lower = style.to_ascii_lowercase();
    // Exact or prefix matches: "listbullet", "listnumber", "list", "list2"…
    let prefixes = ["listbullet", "listnumber", "listparagraph", "list"];
    for prefix in &prefixes {
        if lower == *prefix || lower.starts_with(prefix) {
            return true;
        }
    }
    false
}

fn parse_document_xml_blocks_streaming<R: Read>(reader_src: R) -> Result<Vec<DocxBlock>, String> {
    let mut reader = Reader::from_reader(BufReader::new(reader_src));

    let mut buf = Vec::new();
    let mut blocks: Vec<DocxBlock> = Vec::new();

    let mut in_text = false;
    // One <w:t> can arrive as several events, because quick-xml reports each
    // entity reference separately. Accumulate the element's text verbatim and
    // route it onward once, at </w:t> — joining per event would insert spaces
    // inside words ("AT&amp;T" -> "AT & T").
    let mut wt_buf = String::new();
    let mut in_paragraph = false;

    let mut para_text = String::new();
    let mut para_sub_texts: Vec<String> = Vec::new();
    let mut para_is_list = false;
    let mut para_list_level: u8 = 0;
    let mut para_has_drawing = false;
    let mut para_has_page_break = false;
    let mut para_has_section_break = false;
    let mut para_has_rendered_break = false;
    let mut para_style: Option<String> = None;
    let mut para_outline_lvl: Option<u32> = None;
    let mut para_image_alt: Option<String> = None;
    let mut para_image_rid: Option<String> = None;
    let mut para_images: Vec<(String, Option<String>)> = Vec::new();
    let mut pending_alt: Option<String> = None;
    let mut para_footnote_refs: Vec<String> = Vec::new();
    let mut para_endnote_refs: Vec<String> = Vec::new();
    let mut para_num_id: Option<u32> = None;
    let mut in_hyperlink = false;
    let mut hyperlink_rid = String::new();
    let mut hyperlink_text = String::new();
    let mut para_hyperlinks: Vec<(String, String)> = Vec::new();
    // Depth counter for `<w:drawing>` so we only harvest alt attributes
    // from `<wp:docPr>` / `<pic:cNvPr>` while we are actually inside a
    // drawing (those local names also appear in shape XML elsewhere).
    let mut drawing_depth: u32 = 0;
    let mut in_run = false;
    let mut in_rpr = false;
    let mut cur_bold = false;
    let mut cur_italic = false;
    let mut cur_run_text = String::new();

    // Stack of in-progress tables. Empty when we're at top-level body
    // content. Pushed on `<w:tbl>` Start, popped on `<w:tbl>` End. Nested
    // tables push additional states without disturbing parent state.
    let mut table_stack: Vec<TableState> = Vec::new();
    // Images inside table cells. Cell paragraphs deliberately do not set
    // `in_paragraph` (the table walker owns them), so the paragraph-level blip
    // harvest never sees them and every picture in a table was dropped. (#71)
    let mut table_images: Vec<(String, Option<String>)> = Vec::new();
    let mut table_drawing_depth: i32 = 0;
    let mut table_pending_alt: Option<String> = None;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                if qname_eq(name, b"tbl") {
                    table_stack.push(TableState::default());
                } else if qname_eq(name, b"tr") {
                    if let Some(top) = table_stack.last_mut() {
                        top.current_row.clear();
                        top.in_header_row = false;
                        top.in_tr_pr = false;
                    }
                } else if qname_eq(name, b"trPr") {
                    if let Some(top) = table_stack.last_mut() {
                        top.in_tr_pr = true;
                    }
                } else if qname_eq(name, b"tblHeader") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_tr_pr {
                            top.in_header_row = true;
                        }
                    }
                } else if qname_eq(name, b"tc") {
                    if let Some(top) = table_stack.last_mut() {
                        top.in_cell = true;
                        top.current_cell.clear();
                        top.cell_span = 1;
                        top.cur_cell_is_vmerge_continuation = false;
                    }
                } else if qname_eq(name, b"gridSpan") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let raw =
                                        String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                    if let Ok(v) = raw.trim().parse::<usize>() {
                                        if v > 1 {
                                            top.cell_span = v;
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                } else if qname_eq(name, b"vMerge") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            // Check for w:val="restart" — absent val or val≠"restart" means continuation
                            let mut is_restart = false;
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let v = String::from_utf8_lossy(attr.value.as_ref());
                                    is_restart = v.trim() == "restart";
                                    break;
                                }
                            }
                            top.cur_cell_is_vmerge_continuation = !is_restart;
                        }
                    }
                } else if qname_eq(name, b"p") {
                    if table_stack.is_empty() {
                        in_paragraph = true;
                        para_text.clear();
                        para_sub_texts.clear();
                        para_is_list = false;
                        para_list_level = 0;
                        para_has_drawing = false;
                        para_has_page_break = false;
                        para_has_section_break = false;
                        para_has_rendered_break = false;
                        para_style = None;
                        para_outline_lvl = None;
                        para_image_alt = None;
                        para_image_rid = None;
                        para_images.clear();
                        pending_alt = None;
                        para_footnote_refs.clear();
                        para_endnote_refs.clear();
                        para_num_id = None;
                        in_hyperlink = false;
                        hyperlink_rid.clear();
                        hyperlink_text.clear();
                        para_hyperlinks.clear();
                        drawing_depth = 0;
                        in_run = false;
                        in_rpr = false;
                        cur_bold = false;
                        cur_italic = false;
                        cur_run_text.clear();
                    }
                } else if qname_eq(name, b"numPr") && in_paragraph {
                    para_is_list = true;
                } else if qname_eq(name, b"br") && in_paragraph {
                    let mut is_page = false;
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"type") {
                            let value = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_ascii_lowercase();
                            if value == "page" {
                                para_has_page_break = true;
                                is_page = true;
                            }
                            break;
                        }
                    }
                    if !is_page {
                        // Soft line break — flush accumulated text as a sub-segment.
                        let flushed = std::mem::take(&mut para_text).trim().to_string();
                        if !flushed.is_empty() {
                            para_sub_texts.push(flushed);
                        }
                    }
                } else if qname_eq(name, b"sectPr") && in_paragraph {
                    para_has_section_break = true;
                } else if qname_eq(name, b"lastRenderedPageBreak") && in_paragraph {
                    para_has_rendered_break = true;
                } else if qname_eq(name, b"drawing") && !table_stack.is_empty() {
                    table_drawing_depth = table_drawing_depth.saturating_add(1);
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && table_drawing_depth > 0
                {
                    if let Some(alt) = harvest_image_alt(&e) {
                        table_pending_alt.get_or_insert(alt);
                    }
                } else if qname_eq(name, b"blip") && table_drawing_depth > 0 {
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if !table_images.iter().any(|(r, _)| *r == rid) {
                            table_images.push((rid, table_pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"drawing") && in_paragraph {
                    para_has_drawing = true;
                    drawing_depth = drawing_depth.saturating_add(1);
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && in_paragraph
                    && drawing_depth > 0
                {
                    // Alt text belongs to the drawing it sits in, so remember it
                    // for the next blip rather than only for the paragraph's
                    // first image.
                    if let Some(alt) = harvest_image_alt(&e) {
                        if para_image_alt.is_none() {
                            para_image_alt = Some(alt.clone());
                        }
                        pending_alt.get_or_insert(alt);
                    }
                } else if qname_eq(name, b"blip") && in_paragraph && drawing_depth > 0 {
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if para_image_rid.is_none() {
                            para_image_rid = Some(rid.clone());
                        }
                        // Keep EVERY blip: a paragraph can hold a gallery, and
                        // the first one may be a format we cannot decode. (#13)
                        if !para_images.iter().any(|(r, _)| *r == rid) {
                            para_images.push((rid, pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"footnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_footnote_refs.push(id);
                    }
                } else if qname_eq(name, b"endnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_endnote_refs.push(id);
                    }
                } else if qname_eq(name, b"pStyle") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let v = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            para_style = Some(v);
                            break;
                        }
                    }
                } else if qname_eq(name, b"ilvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u8>() {
                                para_list_level = v;
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"numId") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_num_id = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"hyperlink") && in_paragraph {
                    in_hyperlink = true;
                    hyperlink_text.clear();
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"id") {
                            hyperlink_rid = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            break;
                        }
                    }
                } else if qname_eq(name, b"r") && in_paragraph {
                    in_run = true;
                    cur_bold = false;
                    cur_italic = false;
                    cur_run_text.clear();
                } else if qname_eq(name, b"rPr") && in_run {
                    in_rpr = true;
                } else if qname_eq(name, b"outlineLvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_outline_lvl = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"t") {
                    in_text = true;
                    wt_buf.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                if qname_eq(name, b"numPr") && in_paragraph {
                    para_is_list = true;
                } else if qname_eq(name, b"tblHeader") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_tr_pr {
                            top.in_header_row = true;
                        }
                    }
                } else if qname_eq(name, b"gridSpan") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let raw =
                                        String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                    if let Ok(v) = raw.trim().parse::<usize>() {
                                        if v > 1 {
                                            top.cell_span = v;
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                } else if qname_eq(name, b"vMerge") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            // Check for w:val="restart" — absent val or val≠"restart" means continuation
                            let mut is_restart = false;
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let v = String::from_utf8_lossy(attr.value.as_ref());
                                    is_restart = v.trim() == "restart";
                                    break;
                                }
                            }
                            top.cur_cell_is_vmerge_continuation = !is_restart;
                        }
                    }
                } else if qname_eq(name, b"br") && in_paragraph {
                    let mut is_page = false;
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"type") {
                            let value = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_ascii_lowercase();
                            if value == "page" {
                                para_has_page_break = true;
                                is_page = true;
                            }
                            break;
                        }
                    }
                    if !is_page {
                        let flushed = std::mem::take(&mut para_text).trim().to_string();
                        if !flushed.is_empty() {
                            para_sub_texts.push(flushed);
                        }
                    }
                } else if qname_eq(name, b"sectPr") && in_paragraph {
                    para_has_section_break = true;
                } else if qname_eq(name, b"lastRenderedPageBreak") && in_paragraph {
                    para_has_rendered_break = true;
                } else if qname_eq(name, b"drawing") && in_paragraph {
                    para_has_drawing = true;
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && in_paragraph
                    && drawing_depth > 0
                {
                    // Alt text belongs to the drawing it sits in, so remember it
                    // for the next blip rather than only for the paragraph's
                    // first image.
                    if let Some(alt) = harvest_image_alt(&e) {
                        if para_image_alt.is_none() {
                            para_image_alt = Some(alt.clone());
                        }
                        pending_alt.get_or_insert(alt);
                    }
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && table_drawing_depth > 0
                {
                    if let Some(alt) = harvest_image_alt(&e) {
                        table_pending_alt.get_or_insert(alt);
                    }
                } else if qname_eq(name, b"blip") && table_drawing_depth > 0 {
                    // `<a:blip/>` is self-closing, so it only ever reaches the
                    // Empty arm — the Start-arm branch never sees it. (#71)
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if !table_images.iter().any(|(r, _)| *r == rid) {
                            table_images.push((rid, table_pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"blip") && in_paragraph && drawing_depth > 0 {
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if para_image_rid.is_none() {
                            para_image_rid = Some(rid.clone());
                        }
                        // Keep EVERY blip: a paragraph can hold a gallery, and
                        // the first one may be a format we cannot decode. (#13)
                        if !para_images.iter().any(|(r, _)| *r == rid) {
                            para_images.push((rid, pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"footnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_footnote_refs.push(id);
                    }
                } else if qname_eq(name, b"endnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_endnote_refs.push(id);
                    }
                } else if qname_eq(name, b"pStyle") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let v = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            para_style = Some(v);
                            break;
                        }
                    }
                } else if qname_eq(name, b"ilvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u8>() {
                                para_list_level = v;
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"numId") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_num_id = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"outlineLvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_outline_lvl = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"b") && in_rpr {
                    let mut val = String::new();
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            val = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            break;
                        }
                    }
                    cur_bold = val != "false" && val != "0";
                } else if qname_eq(name, b"i") && in_rpr {
                    let mut val = String::new();
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            val = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            break;
                        }
                    }
                    cur_italic = val != "false" && val != "0";
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                if qname_eq(name, b"t") {
                    in_text = false;
                    let txt = std::mem::take(&mut wt_buf);
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            push_text(&mut top.current_cell, &txt);
                        }
                    } else if in_paragraph {
                        if in_run {
                            push_text(&mut cur_run_text, &txt);
                            if in_hyperlink {
                                push_text(&mut hyperlink_text, &txt);
                            }
                        } else {
                            push_text(&mut para_text, &txt);
                            if in_hyperlink {
                                push_text(&mut hyperlink_text, &txt);
                            }
                        }
                    }
                } else if qname_eq(name, b"drawing") {
                    drawing_depth = drawing_depth.saturating_sub(1);
                    // Only unwind the table counter for drawings that actually
                    // opened inside a table — decrementing on every </w:drawing>
                    // drove it negative, so the `> 0` guard below never passed
                    // and no table image was ever harvested.
                    if table_drawing_depth > 0 {
                        table_drawing_depth -= 1;
                        table_pending_alt = None;
                    }
                } else if qname_eq(name, b"hyperlink") && in_paragraph {
                    let anchor = hyperlink_text.trim().to_string();
                    if !anchor.is_empty() && !hyperlink_rid.is_empty() {
                        para_hyperlinks.push((anchor, hyperlink_rid.clone()));
                    }
                    in_hyperlink = false;
                    hyperlink_rid.clear();
                    hyperlink_text.clear();
                } else if qname_eq(name, b"rPr") {
                    in_rpr = false;
                } else if qname_eq(name, b"r") && in_paragraph {
                    if !cur_run_text.is_empty() {
                        let formatted = match (cur_bold, cur_italic) {
                            (true, true) => format!("***{}***", cur_run_text),
                            (true, false) => format!("**{}**", cur_run_text),
                            (false, true) => format!("*{}*", cur_run_text),
                            (false, false) => cur_run_text.clone(),
                        };
                        push_text(&mut para_text, &formatted);
                    }
                    in_run = false;
                    in_rpr = false;
                    cur_bold = false;
                    cur_italic = false;
                    cur_run_text.clear();
                } else if qname_eq(name, b"trPr") {
                    if let Some(top) = table_stack.last_mut() {
                        top.in_tr_pr = false;
                    }
                } else if qname_eq(name, b"tc") {
                    if let Some(top) = table_stack.last_mut() {
                        let col_index = top.current_row.len();
                        let raw_cell = std::mem::take(&mut top.current_cell).trim().to_string();

                        let cell = if top.cur_cell_is_vmerge_continuation {
                            // Repeat content from the cell above this column position.
                            top.vmerge_col_content
                                .get(col_index)
                                .cloned()
                                .unwrap_or_default()
                        } else {
                            raw_cell.clone()
                        };

                        let span = top.cell_span.max(1);
                        for i in 0..span {
                            // Update vmerge_col_content for each spanned column
                            let abs_col = col_index + i;
                            if !top.cur_cell_is_vmerge_continuation {
                                if top.vmerge_col_content.len() <= abs_col {
                                    top.vmerge_col_content.resize(abs_col + 1, String::new());
                                }
                                top.vmerge_col_content[abs_col] = raw_cell.clone();
                            }
                            top.current_row.push(cell.clone());
                        }

                        top.in_cell = false;
                        top.cell_span = 1;
                        top.cur_cell_is_vmerge_continuation = false;
                    }
                } else if qname_eq(name, b"tr") {
                    if let Some(top) = table_stack.last_mut() {
                        if !top.current_row.is_empty() {
                            let row = std::mem::take(&mut top.current_row);
                            top.header_row_flags.push(top.in_header_row);
                            top.rows.push(row);
                        }
                        top.in_header_row = false;
                        top.in_tr_pr = false;
                    }
                } else if qname_eq(name, b"tbl") {
                    if let Some(state) = table_stack.pop() {
                        if let Some(parent) = table_stack.last_mut() {
                            // Nested table: inline-flatten into the cell
                            // currently being built in the parent.
                            let inline = render_table_inline(&state);
                            if !inline.is_empty() {
                                if !parent.current_cell.is_empty()
                                    && !parent.current_cell.ends_with(' ')
                                {
                                    parent.current_cell.push(' ');
                                }
                                parent.current_cell.push_str(&inline);
                            }
                        } else {
                            let rendered = render_table_markdown(&state);
                            // A table whose cells hold only pictures renders as
                            // empty text — but it still has content. Emitting
                            // nothing here discarded the images with it. (#71)
                            if !rendered.is_empty() || !table_images.is_empty() {
                                blocks.push(DocxBlock {
                                    kind: DocxBlockKind::Table,
                                    text: rendered,
                                    has_drawing: !table_images.is_empty(),
                                    is_list: false,
                                    list_level: 0,
                                    heading_style: None,
                                    outline_level: None,
                                    page_break: false,
                                    section_break: false,
                                    rendered_page_break: false,
                                    image_alt: None,
                                    image_rid: None,
                                    images: std::mem::take(&mut table_images),
                                    footnote_refs: Vec::new(),
                                    endnote_refs: Vec::new(),
                                    num_id: None,
                                    hyperlinks: Vec::new(),
                                });
                            }
                        }
                    }
                } else if qname_eq(name, b"p") {
                    if in_paragraph {
                        // Also treat style-named list paragraphs (e.g. "ListNumber",
                        // "ListBullet") as list items even when <w:numPr> is absent.
                        let style_ref = para_style.as_deref().unwrap_or("");
                        let is_list = para_is_list || is_list_style(style_ref);

                        if para_sub_texts.is_empty() {
                            // Normal case — no soft breaks, emit one block.
                            blocks.push(DocxBlock {
                                kind: DocxBlockKind::Paragraph,
                                text: std::mem::take(&mut para_text),
                                has_drawing: para_has_drawing,
                                is_list,
                                list_level: para_list_level,
                                heading_style: para_style.take(),
                                outline_level: para_outline_lvl.take(),
                                page_break: para_has_page_break,
                                section_break: para_has_section_break,
                                rendered_page_break: para_has_rendered_break,
                                image_alt: para_image_alt.take(),
                                image_rid: para_image_rid.take(),
                            images: std::mem::take(&mut para_images),
                                footnote_refs: std::mem::take(&mut para_footnote_refs),
                                endnote_refs: std::mem::take(&mut para_endnote_refs),
                                num_id: para_num_id,
                                hyperlinks: std::mem::take(&mut para_hyperlinks),
                            });
                        } else {
                            // Paragraph had soft line breaks — emit one block per segment.
                            // Flush any trailing text after the last <w:br/>.
                            let tail = std::mem::take(&mut para_text).trim().to_string();
                            if !tail.is_empty() {
                                para_sub_texts.push(tail);
                            }
                            for (i, sub_text) in para_sub_texts.drain(..).enumerate() {
                                blocks.push(DocxBlock {
                                    kind: DocxBlockKind::Paragraph,
                                    text: sub_text,
                                    // Drawings and break signals only belong to the first
                                    // segment; subsequent ones are plain text continuations.
                                    has_drawing: para_has_drawing && i == 0,
                                    is_list,
                                    list_level: para_list_level,
                                    heading_style: para_style.clone(),
                                    outline_level: para_outline_lvl,
                                    page_break: para_has_page_break && i == 0,
                                    section_break: para_has_section_break && i == 0,
                                    rendered_page_break: para_has_rendered_break && i == 0,
                                    image_alt: if i == 0 { para_image_alt.clone() } else { None },
                                    image_rid: if i == 0 { para_image_rid.clone() } else { None },
                                    images: if i == 0 {
                                        para_images.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    footnote_refs: if i == 0 {
                                        para_footnote_refs.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    endnote_refs: if i == 0 {
                                        para_endnote_refs.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    num_id: para_num_id,
                                    hyperlinks: if i == 0 {
                                        para_hyperlinks.clone()
                                    } else {
                                        Vec::new()
                                    },
                                });
                            }
                            // Clear shared fields after all sub-blocks are emitted.
                            para_style = None;
                            para_outline_lvl = None;
                            para_image_alt = None;
                            para_footnote_refs.clear();
                            para_endnote_refs.clear();
                            para_hyperlinks.clear();
                        }

                        in_paragraph = false;
                        para_is_list = false;
                        para_list_level = 0;
                        para_num_id = None;
                        in_hyperlink = false;
                        hyperlink_rid.clear();
                        hyperlink_text.clear();
                        para_has_drawing = false;
                        para_has_page_break = false;
                        para_has_section_break = false;
                        para_has_rendered_break = false;
                        para_image_rid = None;
                        drawing_depth = 0;
                        in_run = false;
                        in_rpr = false;
                        cur_bold = false;
                        cur_italic = false;
                        cur_run_text.clear();
                    } else if let Some(top) = table_stack.last_mut() {
                        // Separate paragraphs within a table cell with a
                        // single space so multi-paragraph cells stay legible.
                        if top.in_cell
                            && !top.current_cell.is_empty()
                            && !top.current_cell.ends_with(' ')
                        {
                            top.current_cell.push(' ');
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_text {
                    let txt = match t.decode() {
                        Ok(v) => v.into_owned(),
                        Err(_) => String::new(),
                    };
                    wt_buf.push_str(&txt);
                }
            }
            Ok(Event::CData(t)) => {
                if in_text {
                    let txt = String::from_utf8_lossy(t.as_ref());
                    wt_buf.push_str(&txt);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse word/document.xml stream: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(blocks)
}

/// Open a DOCX zip from bytes and read the `r:embed` → image-part-path map from
/// `word/_rels/document.xml.rels`. Shared by the `*_with_images` chunkers.
pub(super) fn open_docx_archive_with_rids(
    bytes: &[u8],
) -> Result<(zip::ZipArchive<std::io::Cursor<Vec<u8>>>, std::collections::HashMap<String, String>), String> {
    use std::io::Read;
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("Not a valid DOCX ZIP: {e}"))?;
    let image_rids_map = match archive.by_name("word/_rels/document.xml.rels") {
        Ok(mut f) => {
            let mut xml = String::new();
            let _ = f.read_to_string(&mut xml);
            parse_rels_xml_images(&xml)
        }
        Err(_) => std::collections::HashMap::new(),
    };
    Ok((archive, image_rids_map))
}

/// From a list of `(rid, alt)` image references, extract deduped image bytes and
/// build `(hash_name, {image_name, alt_text})` chunk entries. Shared by the
/// item-splitting `*_with_images` chunkers (sentence/page_aware/sliding_window).
pub(super) fn collect_image_chunks_from_items(
    image_items: Vec<(Option<String>, Option<String>)>,
    image_rids_map: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
) -> (Vec<(String, serde_json::Value)>, Vec<(String, Vec<u8>)>) {
    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut entries: Vec<(String, serde_json::Value)> = Vec::new();
    for (rid, alt) in image_items {
        let Some(rid) = rid else { continue };
        let Some(zip_path) = image_rids_map.get(&rid) else { continue };
        if let Ok(mut entry) = archive.by_name(zip_path) {
            let mut img_bytes = Vec::new();
            if entry.read_to_end(&mut img_bytes).is_ok() {
                if let Some(hash_name) = image_hash_name(&img_bytes, zip_path) {
                    if !image_out.iter().any(|(n, _)| n == &hash_name) {
                        image_out.push((hash_name.clone(), img_bytes));
                    }
                    let alt_str = alt.as_deref().unwrap_or("");
                    entries.push((
                        hash_name.clone(),
                        serde_json::json!({ "image_name": hash_name, "alt_text": alt_str }),
                    ));
                }
            }
        }
    }
    (entries, image_out)
}
