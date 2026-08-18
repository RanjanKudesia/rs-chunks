use serde_json::{json, Value};

use super::common::{parse_docx_indexed_paragraphs, IndexedParagraph};

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content: String,
    metadata: Value,
}

fn build_sliding_window_chunks(
    paragraphs: Vec<IndexedParagraph>,
    window_size: usize,
    overlap: usize,
) -> Vec<ChunkRecordInput> {
    if paragraphs.is_empty() {
        return Vec::new();
    }

    let step = window_size - overlap;
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut window_index = 0usize;

    while start < paragraphs.len() {
        let end = (start + window_size).min(paragraphs.len());
        let window = &paragraphs[start..end];
        let content = window
            .iter()
            .map(|paragraph| paragraph.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let paragraph_indices: Vec<usize> =
            window.iter().map(|paragraph| paragraph.index).collect();
        let paragraph_meta: Vec<Value> = window
            .iter()
            .map(|p| {
                json!({
                    "index": p.index,
                    "is_heading": p.is_heading,
                    "heading_level": p.heading_level,
                    "is_list": p.is_list,
                    "is_table": p.is_table,
                })
            })
            .collect();
        let heading_count = window.iter().filter(|p| p.is_heading).count();
        let list_count = window.iter().filter(|p| p.is_list).count();

        chunks.push(ChunkRecordInput {
            content,
            metadata: json!({
                "window_size": window_size,
                "overlap": overlap,
                "window_index": window_index,
                "paragraph_indices": paragraph_indices,
                "paragraph_meta": paragraph_meta,
                "heading_count": heading_count,
                "list_item_count": list_count,
                "document_metadata": {
                    "source_type": "docx"
                }
            }),
        });

        if end == paragraphs.len() {
            break;
        }

        start += step;
        window_index += 1;
    }

    chunks
}

pub(super) fn chunk(
    bytes: &[u8],
    window_size: usize,
    overlap: usize,
) -> Result<Vec<crate::chunk::Chunk>, String> {
    let paragraphs = parse_docx_indexed_paragraphs(bytes)?;
    Ok(
        build_sliding_window_chunks(paragraphs, window_size, overlap)
            .into_iter()
            .map(|c| crate::chunk::Chunk::new(c.content, "sliding_window", c.metadata))
            .collect(),
    )
}

pub(super) fn chunk_with_images(
    bytes: &[u8],
    window_size: usize,
    overlap: usize,
) -> Result<crate::chunk::ChunksWithImages, String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let items = super::common::parse_docx_indexed_items_with_images(bytes)?;
    let mut text_paragraphs: Vec<IndexedParagraph> = Vec::new();
    let mut image_items: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut para_index = 0usize;
    for item in items {
        match item {
            super::common::ParaOrImage::Para(ev) => {
                text_paragraphs.push(IndexedParagraph {
                    index: para_index,
                    text: ev.text,
                    is_heading: ev.is_heading,
                    heading_level: ev.heading_level,
                    is_list: ev.is_list,
                    is_table: ev.is_table,
                });
                para_index += 1;
            }
            super::common::ParaOrImage::Image { rid, alt, .. } => image_items.push((rid, alt)),
        }
    }
    let text_chunks = build_sliding_window_chunks(text_paragraphs, window_size, overlap);
    let (entries, image_out) =
        super::common::collect_image_chunks_from_items(image_items, &image_rids_map, &mut archive);
    let mut chunks: Vec<crate::chunk::Chunk> = entries
        .into_iter()
        .map(|(n, m)| crate::chunk::Chunk::new(n, "image", m))
        .collect();
    for c in text_chunks {
        chunks.push(crate::chunk::Chunk::new(
            c.content,
            "sliding_window",
            c.metadata,
        ));
    }
    Ok((chunks, image_out))
}
