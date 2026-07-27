use serde_json::json;

use super::common::{
    collect_slide_names, open_pptx, read_all_slides, ChunkRecordInput,
    ContentType, MAX_CHUNK_CHARS,
};

/// Safety cap: no window chunk may exceed this many chars regardless of window_size.
const MAX_WINDOW_CONTENT_CHARS: usize = MAX_CHUNK_CHARS * 5; // 6000

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

    let mut archive = open_pptx(bytes)?;
    let slide_names = collect_slide_names(&archive);
    if slide_names.is_empty() {
        return Err("No slides found".to_string());
    }
    let total_slides = slide_names.len();

    let mut units: Vec<(usize, String, Option<String>)> = Vec::new();
    for (slide_num, slide) in read_all_slides(&mut archive, &slide_names)? {
        let text = slide.all_text();
        if text.is_empty() {
            continue;
        }
        units.push((slide_num, text, slide.title));
    }

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
        let raw_content = window
            .iter()
            .map(|(_, t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        // Safety cap: prevent unbounded output for very large window_size values.
        let (content, truncated) = if raw_content.len() > MAX_WINDOW_CONTENT_CHARS {
            let budget = crate::shared::floor_char_boundary(
                &raw_content, MAX_WINDOW_CONTENT_CHARS);
            let truncate_at = raw_content[..budget]
                .rfind([' ', '\n'])
                .unwrap_or(budget);
            (raw_content[..truncate_at].trim_end().to_string(), true)
        } else {
            (raw_content, false)
        };
        if !content.is_empty() {
            result.push(ChunkRecordInput {
                content_type: ContentType::SlidingWindow,
                content,
                metadata: json!({
                    "window_size":   window_size,
                    "overlap":       overlap,
                    "window_index":  window_index,
                    "slide_range":   [window[0].0, window.last().unwrap().0],
                    "slide_count":   window.len(),
                    "chunk_index":   window_index,
                    "truncated":     truncated,
                    "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
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

