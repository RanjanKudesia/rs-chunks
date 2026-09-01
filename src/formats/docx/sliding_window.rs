use serde_json::{json, Value};

use super::common::{parse_docx_indexed_paragraphs, IndexedParagraph};

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content: String,
    metadata: Value,
}

/// Safety cap on assembled window content, independent of `window_size`.
///
/// A window is `window_size` PARAGRAPHS, and a whole table is one paragraph —
/// so `poi_bug65649.docx` produced a single 886,428-char chunk. Mirrors pptx's
/// window cap.
const MAX_WINDOW_CONTENT_CHARS: usize = 6_000;

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
        let raw_content = window
            .iter()
            .map(|paragraph| paragraph.text.as_str())
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

        // Sentences, not lines: on this path a table's newlines have already
        // been collapsed to spaces, so the 522,261-char table arrives as ONE
        // line and `split_block_on_lines` alone would not touch it.
        // `split_block_on_lines_and_sentences` falls through to a word-boundary
        // hard split, which bounds it for real.
        //
        // Inert below the cap: content shorter than the cap is returned
        // unchanged by both stages, and only three corpus fixtures exceed it.
        // This is NOT the excluded semantic sentence-splitter swap, which would
        // move prose boundaries on every docx.
        for content in crate::shared::split_block_on_lines_and_sentences(
            &raw_content,
            MAX_WINDOW_CONTENT_CHARS,
        ) {
            // The splitter can yield an empty part (an all-whitespace window,
            // or a trailing separator). An empty chunk violates the standard
            // schema — `content` must be a non-empty string — which pytest
            // enforces and the golden snapshot does not.
            if content.trim().is_empty() {
                continue;
            }
            chunks.push(ChunkRecordInput {
                content,
                metadata: json!({
                    "window_size": window_size,
                    "overlap": overlap,
                    // Per WINDOW, not per emitted chunk. The published contract
                    // (`chunking-modes/sliding-window.mdx`: "which window this
                    // is, 0-based") and all six sibling modules — txt, md,
                    // html, pptx, xlsx, csv — count windows, so the parts of a
                    // size-split window share one index. An uncommitted draft
                    // briefly counted emitted chunks instead, citing an "SDK
                    // contract asserted by test_window_index_increments_from_
                    // zero" — a test that did not exist anywhere in the
                    // workspace. It does now (tests/docx_window_index.rs), and
                    // it pins THIS semantics.
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
        }

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
