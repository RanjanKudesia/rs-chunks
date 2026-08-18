/// Page-aware chunker for PPTX.
/// Groups N consecutive slides into one chunk.  Slides are already discrete
/// pages, so `paragraphs_per_page` is interpreted as `slides_per_chunk`.
use serde_json::json;

use super::common::{
    collect_slide_names, open_pptx, read_all_slides, ChunkRecordInput, ContentType,
};

pub fn build_page_aware_chunks(
    bytes: &[u8],
    slides_per_chunk: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    if slides_per_chunk == 0 {
        return Err("slides_per_chunk must be > 0".to_string());
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

    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut chunk_index = 0usize;
    let mut i = 0usize;
    while i < units.len() {
        let end = (i + slides_per_chunk).min(units.len());
        let window = &units[i..end];
        let content = window
            .iter()
            .map(|(_, t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !content.is_empty() {
            result.push(ChunkRecordInput {
                content_type: ContentType::PageAware,
                content,
                metadata: json!({
                    "slides_per_chunk": slides_per_chunk,
                    "slide_count":      window.len(),
                    "slide_range":      [window[0].0, window.last().unwrap().0],
                    "page_break_type":  "slide_boundary",
                    "chunk_index":      chunk_index,
                    "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
                }),
            });
            chunk_index += 1;
        }
        i = end;
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
