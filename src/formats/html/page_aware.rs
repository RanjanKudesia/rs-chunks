use serde_json::json;

use super::common::{
    current_section_heading, heading_path_strings, parse_html_blocks, remove_comments,
    update_heading_stack, ChunkRecordInput, ContentType, HtmlBlockType,
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
        return Err("paragraphs_per_page must be > 0".to_string());
    }
    let text = super::encoding::decode_html(bytes);
    if text.trim().is_empty() {
        return Err("HTML file is empty".to_string());
    }
    let blocks = parse_html_blocks(&remove_comments(&text));
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
                let pc = a.len();
                let bt = a.break_type;
                let sh = a.section_heading.clone();
                let hp = a.heading_path.clone();
                let content = a.into_content();
                result.push(ChunkRecordInput {
                    content_type: ContentType::PageAware, content,
                    metadata: json!({ "page_break_type": bt, "paragraph_count": pc, "section_heading": sh, "heading_path": hp, "chunk_index": *ci, "document_metadata": { "source_type": "html", "total_input_blocks": total } }),
                });
                *ci += 1;
            }
        }
    };

    for block in &blocks {
        if block.block_type == HtmlBlockType::Heading {
            flush(&mut accum, &mut result, &mut chunk_index, total);
            update_heading_stack(
                &mut heading_stack,
                block.heading_level,
                block.content.clone(),
            );
            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection, content: block.content.clone(),
                metadata: json!({ "page_break_type": "heading_boundary", "paragraph_count": 0, "section_heading": block.content, "section_level": block.heading_level, "heading_path": heading_path_strings(&heading_stack), "chunk_index": chunk_index, "document_metadata": { "source_type": "html", "total_input_blocks": total } }),
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
