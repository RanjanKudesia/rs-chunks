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
    ContentType, MdBlockType,
};

// ── Unit record ───────────────────────────────────────────────────────────────

struct BlockUnit {
    content: String,
    section_heading: Option<String>,
    heading_path: Vec<String>,
}

// ── Core algorithm ────────────────────────────────────────────────────────────

pub fn build_sliding_window_chunks(
    bytes: &[u8],
    window_size: usize,
    overlap: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    if window_size == 0 {
        return Err("window_size must be greater than 0".to_string());
    }
    if overlap >= window_size {
        return Err("overlap must be less than window_size".to_string());
    }

    let text = std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string());
    if text.trim().is_empty() {
        return Err("Markdown file is empty".to_string());
    }

    let blocks = parse_markdown_blocks(&text);
    let total_input_blocks = blocks.len();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();

    // Convert every block into a text unit, tracking heading context.
    let mut units: Vec<BlockUnit> = Vec::new();
    for block in blocks {
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
        });
    }

    if units.is_empty() {
        return Err("No content blocks found".to_string());
    }

    let step = window_size - overlap;
    let mut result: Vec<ChunkRecordInput> = Vec::new();
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
            result.push(ChunkRecordInput {
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
            });
            window_index += 1;
        }

        if end >= units.len() {
            break;
        }
        start += step;
    }

    if result.is_empty() {
        return Err("No sliding-window chunks generated".to_string());
    }
    Ok(result)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────

