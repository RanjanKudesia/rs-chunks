use serde_json::json;

use super::common::{
    current_section_heading, extract_heading_text, heading_level_txt, heading_path_strings,
    parse_txt_blocks, update_heading_stack, ChunkRecordInput, ContentType,
};

struct PageAccum {
    parts: Vec<String>,
    section_heading: Option<String>,
    heading_path: Vec<String>,
    break_type: &'static str,
}

impl PageAccum {
    fn new(sh: Option<String>, hp: Vec<String>, bt: &'static str) -> Self {
        PageAccum {
            parts: Vec::new(),
            section_heading: sh,
            heading_path: hp,
            break_type: bt,
        }
    }
    fn push(&mut self, s: String) {
        self.parts.push(s);
    }
    fn len(&self) -> usize {
        self.parts.len()
    }
    fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
    fn into_content(self) -> String {
        self.parts.join("\n\n")
    }
}

pub fn build_page_aware_chunks(
    bytes: &[u8],
    paragraphs_per_page: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    if paragraphs_per_page == 0 {
        return Err("paragraphs_per_page must be greater than 0".to_string());
    }
    let text = crate::text_encoding::decode_text(bytes).0;
    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_txt_blocks(&text);
    let total = blocks.len();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut accum: Option<PageAccum> = None;
    let mut chunk_index = 0usize;

    let flush = |accum: &mut Option<PageAccum>,
                 result: &mut Vec<ChunkRecordInput>,
                 ci: &mut usize,
                 total: usize| {
        if let Some(a) = accum.take() {
            if !a.is_empty() {
                let paragraph_count = a.len();
                let break_type = a.break_type;
                let sh = a.section_heading.clone();
                let hp = a.heading_path.clone();
                let content = a.into_content();
                result.push(ChunkRecordInput {
                    content_type: ContentType::PageAware,
                    content,
                    metadata: json!({
                        "page_break_type":   break_type,
                        "paragraph_count":   paragraph_count,
                        "section_heading":   sh,
                        "heading_path":      hp,
                        "chunk_index":       *ci,
                        "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                    }),
                });
                *ci += 1;
            }
        }
    };

    for block in &blocks {
        if block.content_type == ContentType::HeadingSection {
            flush(&mut accum, &mut result, &mut chunk_index, total);
            let level = heading_level_txt(&block.content);
            let text = extract_heading_text(&block.content);
            update_heading_stack(&mut heading_stack, level, text.clone());
            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection,
                content: text.clone(),
                metadata: json!({
                    "page_break_type":  "heading_boundary",
                    "paragraph_count":  0,
                    "section_heading":  text,
                    "section_level":    level,
                    "heading_path":     heading_path_strings(&heading_stack),
                    "chunk_index":      chunk_index,
                    "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                }),
            });
            chunk_index += 1;
            accum = Some(PageAccum::new(
                current_section_heading(&heading_stack),
                heading_path_strings(&heading_stack),
                "heading_boundary",
            ));
        } else {
            let a = accum.get_or_insert_with(|| {
                PageAccum::new(
                    current_section_heading(&heading_stack),
                    heading_path_strings(&heading_stack),
                    "estimated",
                )
            });
            a.push(block.content.clone());
            if a.len() >= paragraphs_per_page {
                flush(&mut accum, &mut result, &mut chunk_index, total);
            }
        }
    }
    flush(&mut accum, &mut result, &mut chunk_index, total);
    // Empty is not a failure (TECH_DEBT T6): the document parsed, this mode
    // simply produced nothing. Returning `[]` keeps every mode consistent with
    // docx/ppt/xlsx and lets epub distinguish an empty chapter from a broken
    // one without swallowing errors (L14).
    if result.is_empty() {
        return Ok(Vec::new());
    }
    Ok(result)
}
