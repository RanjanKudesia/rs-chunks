/// Sliding-window chunker for Markdown.
///
/// Treats every block (heading, paragraph, list, code, table) as a unit and
/// builds overlapping windows of `window_size` consecutive units.  Each window
/// is one chunk.  The step between windows is `window_size - overlap`.
///
/// Metadata per chunk:
///   window_size      — number of blocks per window
///   overlap          — blocks shared between consecutive windows
///   window_index     — 0-based position of this window
///   paragraph_range  — [start_block_index, end_block_index]
///   section_heading  — heading context at the start of the window
///   heading_path     — full ancestor breadcrumb at window start
use serde_json::json;

use super::common::{
    current_section_heading, extract_heading_text, heading_level, heading_path_strings,
    parse_markdown_blocks, strip_block_content, update_heading_stack, ChunkRecordInput,
    ContentType, MdBlockType, SpannedRecord,
};

// ── Unit record ───────────────────────────────────────────────────────────────

struct BlockUnit {
    content: String,
    section_heading: Option<String>,
    heading_path: Vec<String>,
    /// The source block this unit came from. Not the same as the unit's own
    /// position: empty blocks are skipped, so `paragraph_range` counts units
    /// while record provenance needs blocks.
    block: usize,
}

// ── Core algorithm ────────────────────────────────────────────────────────────

pub fn build_sliding_window_chunks(
    bytes: &[u8],
    window_size: usize,
    overlap: usize,
) -> Result<Vec<SpannedRecord>, String> {
    if window_size == 0 {
        return Err("window_size must be greater than 0".to_string());
    }
    if overlap >= window_size {
        return Err("overlap must be less than window_size".to_string());
    }

    let text = super::common::decode_body(bytes);
    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_markdown_blocks(&text);
    let total_input_blocks = blocks.len();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();

    // Convert every block into a text unit, tracking heading context.
    let mut units: Vec<BlockUnit> = Vec::new();
    for block in blocks {
        let index = block.index;
        let content = match block.block_type {
            MdBlockType::Heading => {
                let level = heading_level(&block.content);
                let text = extract_heading_text(&block.content);
                update_heading_stack(&mut heading_stack, level, text.clone());
                text
            }
            MdBlockType::Paragraph => {
                let c = strip_block_content(&block.content, false);
                if c.is_empty() {
                    continue;
                }
                c
            }
            MdBlockType::List => {
                let c = strip_block_content(&block.content, true);
                if c.is_empty() {
                    continue;
                }
                c
            }
            MdBlockType::Code | MdBlockType::Table => block.content.clone(),
        };
        units.push(BlockUnit {
            content,
            section_heading: current_section_heading(&heading_stack),
            heading_path: heading_path_strings(&heading_stack),
            block: index,
        });
    }

    // Same contract as the structural builders (TECH_DEBT T6): a document that
    // parsed cleanly but holds no content blocks is EMPTY, not broken. Returning
    // an error here is what made epub's per-chapter swallow necessary — an
    // image-only cover page hit this path (L14).
    if units.is_empty() {
        return Ok(Vec::new());
    }

    let step = window_size - overlap;
    let mut result: Vec<SpannedRecord> = Vec::new();
    let mut start = 0usize;
    let mut window_index = 0usize;

    loop {
        let end = (start + window_size).min(units.len());
        let window = &units[start..end];

        let content = window
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        if !content.is_empty() {
            let span = Some((window[0].block, window[window.len() - 1].block));
            result.push(SpannedRecord::spanning(
                ChunkRecordInput {
                    content_type: ContentType::SlidingWindow,
                    content,
                    metadata: json!({
                        "window_size":      window_size,
                        "overlap":          overlap,
                        "window_index":     window_index,
                        "paragraph_range":  [start, end.saturating_sub(1)],
                        "block_count":      window.len(),
                        "section_heading":  window[0].section_heading,
                        "heading_path":     window[0].heading_path,
                        "chunk_index":      window_index,
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                },
                span,
            ));
            window_index += 1;
        }

        if end >= units.len() {
            break;
        }
        start += step;
    }

    // Empty is not a failure (TECH_DEBT T6): the document parsed, this mode
    // simply produced nothing. Returning `[]` keeps every mode consistent with
    // docx/ppt/xlsx and lets epub distinguish an empty chapter from a broken
    // one without swallowing errors (L14).
    if result.is_empty() {
        return Ok(Vec::new());
    }
    Ok(result)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────
