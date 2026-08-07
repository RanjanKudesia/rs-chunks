//! The `parse_docx_*` entry points every DOCX mode calls.

use std::io::Cursor;

use zip::ZipArchive;

use super::block_model::{
    docx_heading_level, DocxBlock, DocxBlockKind, IndexedParagraph, PageBreakSignal,
    ParagraphEvent, MIN_PARAGRAPH_CHARS,
};
use super::images_rels::image_placeholder;
use super::stream_walker::parse_document_xml_blocks_streaming;
use super::xml_text::collapse_whitespace;

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
