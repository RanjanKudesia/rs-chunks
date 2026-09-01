use serde_json::{json, Value};

use super::common::{parse_docx_paragraph_events, PageBreakSignal, ParagraphEvent};

const MAX_PAGE_AWARE_CHUNK_CHARS: usize = 2000;

fn page_break_signal_str(signal: PageBreakSignal) -> &'static str {
    match signal {
        PageBreakSignal::Explicit => "explicit",
        PageBreakSignal::Section => "section",
        PageBreakSignal::Rendered => "rendered",
        PageBreakSignal::None => "estimated",
    }
}

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content: String,
    metadata: Value,
}

fn build_page_aware_chunks(
    events: Vec<ParagraphEvent>,
    paragraphs_per_page: usize,
) -> Vec<ChunkRecordInput> {
    if events.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut page_number = 1usize;
    let mut current_paragraphs: Vec<String> = Vec::new();
    let mut current_headings: Vec<Value> = Vec::new();
    let mut current_section_heading_level: Option<u32> = None;
    let mut current_list_items = 0usize;
    let mut current_table_count = 0usize;
    let mut paragraph_count = 0usize;

    for event in events {
        if event.is_heading {
            current_headings.push(json!({
                "level": event.heading_level,
                "text": event.text,
            }));
            if current_section_heading_level.is_none() {
                current_section_heading_level = event.heading_level;
            }
        }
        if event.is_list {
            current_list_items += 1;
        }
        if event.is_table {
            current_table_count += 1;
        }
        current_paragraphs.push(event.text);
        paragraph_count += 1;

        let boundary_signal = match event.signal {
            PageBreakSignal::Explicit => Some(PageBreakSignal::Explicit),
            PageBreakSignal::Section => Some(PageBreakSignal::Section),
            PageBreakSignal::Rendered => Some(PageBreakSignal::Rendered),
            PageBreakSignal::None if paragraph_count >= paragraphs_per_page => {
                Some(PageBreakSignal::None)
            }
            PageBreakSignal::None => None,
        };

        if let Some(signal) = boundary_signal {
            chunks.extend(build_page_chunk(
                &current_paragraphs,
                page_number,
                signal,
                paragraph_count,
                &current_headings,
                current_section_heading_level,
                current_list_items,
                current_table_count,
            ));
            page_number += 1;
            current_paragraphs.clear();
            current_headings.clear();
            current_section_heading_level = None;
            current_list_items = 0;
            current_table_count = 0;
            paragraph_count = 0;
        }
    }

    if !current_paragraphs.is_empty() {
        chunks.extend(build_page_chunk(
            &current_paragraphs,
            page_number,
            PageBreakSignal::None,
            paragraph_count,
            &current_headings,
            current_section_heading_level,
            current_list_items,
            current_table_count,
        ));
    }

    chunks
}

// Assembles one chunk from eight already-computed, unrelated values.
#[allow(clippy::too_many_arguments)]
fn build_page_chunk(
    paragraphs: &[String],
    page_number: usize,
    break_type: PageBreakSignal,
    paragraph_count: usize,
    headings: &[Value],
    section_heading_level: Option<u32>,
    list_item_count: usize,
    table_count: usize,
) -> Vec<ChunkRecordInput> {
    let content = paragraphs.join("\n\n");
    let pieces = split_page_content(&content, MAX_PAGE_AWARE_CHUNK_CHARS);
    let total = pieces.len();

    pieces
        .into_iter()
        .enumerate()
        .map(|(idx, piece)| {
            let mut metadata = json!({
                "page_number": page_number,
                "page_break_type": page_break_signal_str(break_type),
                "paragraph_count": paragraph_count,
                "headings": headings,
                "section_heading_level": section_heading_level,
                "list_item_count": list_item_count,
                "table_count": table_count,
                "document_metadata": {
                    "source_type": "docx"
                }
            });

            if total > 1 {
                if let Some(meta_obj) = metadata.as_object_mut() {
                    meta_obj.insert("chunk_index".to_string(), json!(idx + 1));
                    meta_obj.insert("total_chunks".to_string(), json!(total));
                }
            }

            ChunkRecordInput {
                content: piece,
                metadata,
            }
        })
        .collect()
}

fn split_page_content(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for paragraph in text.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }

        if paragraph.len() > max_chars {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
                current.clear();
            }
            out.extend(split_long_text(paragraph, max_chars));
            continue;
        }

        let candidate = if current.is_empty() {
            paragraph.to_string()
        } else {
            format!("{}\n\n{}", current, paragraph)
        };

        if candidate.len() > max_chars {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
            }
            current = paragraph.to_string();
        } else {
            current = candidate;
        }
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    if out.is_empty() {
        vec![text.to_string()]
    } else {
        out
    }
}

fn split_long_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if !current.is_empty() && current.len() + ch.len_utf8() > max_chars {
            out.push(current.trim().to_string());
            current.clear();
        }
        current.push(ch);
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    if out.is_empty() {
        vec![text.to_string()]
    } else {
        out
    }
}

pub(super) fn chunk(
    bytes: &[u8],
    paragraphs_per_page: usize,
) -> Result<Vec<crate::chunk::Chunk>, String> {
    let events = parse_docx_paragraph_events(bytes)?;
    Ok(build_page_aware_chunks(events, paragraphs_per_page)
        .into_iter()
        .map(|c| crate::chunk::Chunk::new(c.content, "page_aware", c.metadata))
        .collect())
}

pub(super) fn chunk_with_images(
    bytes: &[u8],
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages, String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let items = super::common::parse_docx_paragraph_events_with_images(bytes)?;
    let mut text_events: Vec<ParagraphEvent> = Vec::new();
    let mut image_items: Vec<(Option<String>, Option<String>)> = Vec::new();
    for item in items {
        match item {
            super::common::ParaOrImage::Para(ev) => text_events.push(ev),
            super::common::ParaOrImage::Image { rid, alt, signal } => {
                if !matches!(signal, PageBreakSignal::None) {
                    if let Some(last) = text_events.last_mut() {
                        if matches!(last.signal, PageBreakSignal::None) {
                            last.signal = signal;
                        }
                    }
                }
                image_items.push((rid, alt));
            }
        }
    }
    let text_chunks = build_page_aware_chunks(text_events, paragraphs_per_page);
    let (entries, image_out) =
        super::common::collect_image_chunks_from_items(image_items, &image_rids_map, &mut archive);
    let mut chunks: Vec<crate::chunk::Chunk> = entries
        .into_iter()
        .map(|(n, m)| crate::chunk::Chunk::new(n, "image", m))
        .collect();
    for c in text_chunks {
        chunks.push(crate::chunk::Chunk::new(
            c.content,
            "page_aware",
            c.metadata,
        ));
    }
    Ok((chunks, image_out))
}
