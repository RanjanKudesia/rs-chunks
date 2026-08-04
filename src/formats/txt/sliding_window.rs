use serde_json::json;

use super::common::{
    current_section_heading, extract_heading_text, heading_level_txt, heading_path_strings,
    parse_txt_blocks, update_heading_stack, ChunkRecordInput, ContentType,
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
    if window_size == 0 { return Err("window_size must be greater than 0".to_string()); }
    if overlap >= window_size { return Err("overlap must be less than window_size".to_string()); }

    let text = crate::text_encoding::decode_text(bytes).0;
    if text.trim().is_empty() { return Err("TXT file is empty".to_string()); }

    let blocks = parse_txt_blocks(&text);
    let total = blocks.len();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut units: Vec<BlockUnit> = Vec::new();

    for block in &blocks {
        let content = if block.content_type == ContentType::HeadingSection {
            let level = heading_level_txt(&block.content);
            let text = extract_heading_text(&block.content);
            update_heading_stack(&mut heading_stack, level, text.clone());
            text
        } else {
            block.content.trim().to_string()
        };
        if content.is_empty() { continue; }
        units.push(BlockUnit {
            content,
            section_heading: current_section_heading(&heading_stack),
            heading_path: heading_path_strings(&heading_stack),
        });
    }

    if units.is_empty() { return Err("No content blocks found".to_string()); }

    let step = window_size - overlap;
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut start = 0usize;
    let mut window_index = 0usize;

    loop {
        let end = (start + window_size).min(units.len());
        let window = &units[start..end];
        let content = window.iter().map(|u| u.content.as_str()).collect::<Vec<_>>().join("\n\n");
        if !content.is_empty() {
            result.push(ChunkRecordInput {
                content_type: ContentType::SlidingWindow,
                content,
                metadata: json!({
                    "window_size":      window_size,
                    "overlap":          overlap,
                    "window_index":     window_index,
                    "paragraph_range":  [start, end.saturating_sub(1)],
                    "paragraph_count":  window.len(),
                    "section_heading":  window[0].section_heading,
                    "heading_path":     window[0].heading_path,
                    "chunk_index":      window_index,
                    "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                }),
            });
            window_index += 1;
        }
        if end >= units.len() { break; }
        start += step;
    }

    if result.is_empty() { return Err("No sliding-window chunks generated".to_string()); }
    Ok(result)
}

