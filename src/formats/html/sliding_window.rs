use serde_json::json;

use super::common::{
    current_section_heading, heading_path_strings, parse_html_blocks, remove_comments,
    update_heading_stack, ChunkRecordInput, ContentType, HtmlBlockType,
};

struct BlockUnit {
    content: String,
    section_heading: Option<String>,
    heading_path: Vec<String>,
}

pub fn build_sliding_window_chunks(
    bytes: &[u8],
    window_size: usize,
    overlap: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    if window_size == 0 {
        return Err("window_size must be > 0".to_string());
    }
    if overlap >= window_size {
        return Err("overlap must be < window_size".to_string());
    }
    let text = super::encoding::decode_html(bytes);
    if text.trim().is_empty() {
        return Err("HTML file is empty".to_string());
    }
    let blocks = parse_html_blocks(&remove_comments(&text));
    let total = blocks.len();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut units: Vec<BlockUnit> = Vec::new();
    for block in &blocks {
        if block.block_type == HtmlBlockType::Heading {
            update_heading_stack(
                &mut heading_stack,
                block.heading_level,
                block.content.clone(),
            );
        }
        let content = block.content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        units.push(BlockUnit {
            content,
            section_heading: current_section_heading(&heading_stack),
            heading_path: heading_path_strings(&heading_stack),
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
                content_type: ContentType::SlidingWindow, content,
                metadata: json!({
                    "window_size": window_size, "overlap": overlap, "window_index": window_index,
                    "paragraph_range": [start, end.saturating_sub(1)], "paragraph_count": window.len(),
                    "section_heading": window[0].section_heading, "heading_path": window[0].heading_path,
                    "chunk_index": window_index, "document_metadata": { "source_type": "html", "total_input_blocks": total }
                }),
            });
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
