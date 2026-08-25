/// Section chunker for HTML.
///
/// Uses the explicit h1-h6 heading hierarchy — no heuristics needed.
/// Each heading starts a new section; all blocks until the next heading
/// accumulate as that section's body.  Sections > MAX_SECTION_CHARS are split.
/// Metadata includes full `heading_path` breadcrumb.
use serde_json::json;

use super::common::{
    current_section_level, heading_path_strings, parse_html_blocks, remove_comments,
    split_at_sentences, update_heading_stack, ChunkRecordInput, ContentType, HtmlBlockType,
};

const MAX_SECTION_CHARS: usize = 2000;

struct SectionBody {
    parts: Vec<(String, &'static str)>,
    section_heading: String,
    section_level: u8,
    heading_path: Vec<String>,
}
impl SectionBody {
    fn char_count(&self) -> usize {
        self.parts.iter().map(|(c, _)| c.len()).sum::<usize>()
            + self.parts.len().saturating_sub(1) * 2
    }
    fn paragraph_count(&self) -> usize {
        self.parts
            .iter()
            .filter(|(_, t)| *t == "paragraph" || *t == "list")
            .count()
    }
    fn block_types(&self) -> Vec<&'static str> {
        let mut seen: Vec<&'static str> = Vec::new();
        for (_, t) in &self.parts {
            if !seen.contains(t) {
                seen.push(t);
            }
        }
        seen
    }
    fn joined(&self) -> String {
        self.parts
            .iter()
            .map(|(c, _)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

fn split_section(text: &str, max: usize) -> Vec<String> {
    if text.len() <= max {
        return vec![text.trim().to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        let candidate = if current.is_empty() {
            para.to_string()
        } else {
            format!("{}\n\n{}", current, para)
        };
        if candidate.len() <= max {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(current.trim().to_string());
                current = String::new();
            }
            if para.len() > max {
                // Para alone exceeds the limit — split it into sentence-sized
                // pieces and flush ALL of them, not just the first one.
                for piece in split_at_sentences(para, max) {
                    if !piece.is_empty() {
                        chunks.push(piece);
                    }
                }
            } else {
                current = para.to_string();
            }
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() {
        vec![text.trim().to_string()]
    } else {
        chunks
    }
}

fn flush_section(
    result: &mut Vec<ChunkRecordInput>,
    body: SectionBody,
    ci: &mut usize,
    total: usize,
) {
    let joined = body.joined();
    if joined.trim().is_empty() {
        return;
    }
    let block_types = body.block_types();
    let paragraph_count = body.paragraph_count();
    let parts = split_section(&joined, MAX_SECTION_CHARS);
    let part_count = parts.len();
    for (i, content) in parts.into_iter().enumerate() {
        if content.is_empty() {
            continue;
        }
        result.push(ChunkRecordInput {
            content_type: ContentType::Section, content: content.clone(),
            metadata: json!({
                "section_heading": body.section_heading, "section_level": body.section_level,
                "heading_path": body.heading_path, "paragraph_count": paragraph_count,
                "block_types": block_types, "char_count": content.len(),
                "split_part": if part_count > 1 { json!(i+1) } else { serde_json::Value::Null },
                "split_total": if part_count > 1 { json!(part_count) } else { serde_json::Value::Null },
                "chunk_index": *ci,
                "document_metadata": { "source_type": "html", "total_input_blocks": total }
            }),
        });
        *ci += 1;
    }
}

fn block_type_str(bt: HtmlBlockType) -> &'static str {
    match bt {
        HtmlBlockType::Paragraph => "paragraph",
        HtmlBlockType::List => "list",
        HtmlBlockType::Code => "code_block",
        HtmlBlockType::Table => "table",
        HtmlBlockType::Heading => "heading",
    }
}

pub fn build_section_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let text = super::encoding::decode_html(bytes);
    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let blocks = parse_html_blocks(&remove_comments(&text));
    let total = blocks.len();
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut current: Option<SectionBody> = None;
    let mut chunk_index = 0usize;

    for block in &blocks {
        if block.block_type == HtmlBlockType::Heading {
            if let Some(body) = current.take() {
                flush_section(&mut result, body, &mut chunk_index, total);
            }
            let level = block.heading_level;
            let text = block.content.clone();
            update_heading_stack(&mut heading_stack, level, text.clone());
            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection,
                content: text.clone(),
                metadata: json!({
                    "section_heading": text.clone(), "section_level": level,
                    "heading_path": heading_path_strings(&heading_stack),
                    "paragraph_count": 0, "block_types": ["heading"], "chunk_index": chunk_index,
                    "document_metadata": { "source_type": "html", "total_input_blocks": total }
                }),
            });
            chunk_index += 1;
            current = Some(SectionBody {
                parts: Vec::new(),
                section_heading: text,
                section_level: level,
                heading_path: heading_path_strings(&heading_stack),
            });
        } else {
            let bts = block_type_str(block.block_type);
            let a = current.get_or_insert_with(|| SectionBody {
                parts: Vec::new(),
                section_heading: "Preamble".to_string(),
                section_level: current_section_level(&heading_stack),
                heading_path: heading_path_strings(&heading_stack),
            });
            if a.char_count() + block.content.len() + 2 > MAX_SECTION_CHARS && !a.parts.is_empty() {
                let next = (
                    a.section_heading.clone(),
                    a.section_level,
                    a.heading_path.clone(),
                );
                let body = current.take().unwrap();
                flush_section(&mut result, body, &mut chunk_index, total);
                current = Some(SectionBody {
                    parts: Vec::new(),
                    section_heading: next.0,
                    section_level: next.1,
                    heading_path: next.2,
                });
                current
                    .as_mut()
                    .unwrap()
                    .parts
                    .push((block.content.clone(), bts));
            } else {
                a.parts.push((block.content.clone(), bts));
            }
        }
    }
    if let Some(body) = current.take() {
        flush_section(&mut result, body, &mut chunk_index, total);
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
