//! The block / paragraph-event model emitted by the DOCX walkers.

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
    /// `r:id` of a `<w:altChunk>` (ECMA-376 §17.17.2.1) standing at this
    /// block's position.
    ///
    /// The block is a placeholder: `parse_docx_blocks` resolves the
    /// relationship, converts the imported part, and replaces it in place.
    /// The walker cannot do that itself — it is handed a `Read` over the main
    /// part and has no archive handle.
    ///
    /// A `<w:altChunk>` is a body-level sibling of `<w:p>`, so a document whose
    /// body is only an altChunk produced no blocks at all and every mode
    /// returned 0 chunks, with no error.
    pub alt_chunk_rid: Option<String>,
}
