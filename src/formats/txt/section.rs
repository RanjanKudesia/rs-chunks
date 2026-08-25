/// Section chunker for plain text.
///
/// Detects heading blocks heuristically (ALL CAPS, setext underline, ATX #)
/// and groups every block that follows into one section chunk until the next
/// heading.  Sections exceeding MAX_SECTION_CHARS are split at paragraph
/// boundaries.
///
/// Metadata per chunk:
///   section_heading  — heading text that opened this section
///   section_level    — inferred from underline style / ATX depth
///   heading_path     — full ancestor breadcrumb
///   paragraph_count  — non-heading blocks accumulated
///   block_types      — unique block types present
///   char_count       — total content characters
use serde_json::json;

use super::common::{
    current_section_level, extract_heading_text, heading_level_txt, heading_path_strings,
    parse_txt_blocks, update_heading_stack, ChunkRecordInput, ContentType,
};

const MAX_SECTION_CHARS: usize = 2000;

// ── Section body accumulator ──────────────────────────────────────────────────

struct SectionBody {
    parts: Vec<(String, &'static str)>,
    section_heading: String,
    section_level: u8,
    heading_path: Vec<String>,
}

impl SectionBody {
    fn joined(&self) -> String {
        self.parts
            .iter()
            .map(|(c, _)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    fn char_count(&self) -> usize {
        self.parts.iter().map(|(c, _)| c.len()).sum::<usize>()
            + self.parts.len().saturating_sub(1) * 2
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
    fn paragraph_count(&self) -> usize {
        self.parts
            .iter()
            .filter(|(_, t)| *t == "paragraph" || *t == "list")
            .count()
    }
}

fn split_large_section(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
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
        if candidate.len() <= max_chars {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(current.trim().to_string());
            }
            current = para.to_string();
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
    chunk_index: &mut usize,
    total: usize,
) {
    let joined = body.joined();
    if joined.trim().is_empty() {
        return;
    }
    let block_types = body.block_types();
    let paragraph_count = body.paragraph_count();
    let parts = split_large_section(&joined, MAX_SECTION_CHARS);
    let part_count = parts.len();
    for (i, content) in parts.into_iter().enumerate() {
        if content.is_empty() {
            continue;
        }
        result.push(ChunkRecordInput {
            content_type: ContentType::Section,
            content: content.clone(),
            metadata: json!({
                "section_heading":  body.section_heading,
                "section_level":    body.section_level,
                "heading_path":     body.heading_path,
                "paragraph_count":  paragraph_count,
                "block_types":      block_types,
                "char_count":       content.len(),
                "split_part":       if part_count > 1 { json!(i + 1) } else { serde_json::Value::Null },
                "split_total":      if part_count > 1 { json!(part_count) } else { serde_json::Value::Null },
                "chunk_index":      *chunk_index,
                "document_metadata": { "source_type": "txt", "total_input_blocks": total }
            }),
        });
        *chunk_index += 1;
    }
}

fn block_type_str(ct: ContentType) -> &'static str {
    match ct {
        ContentType::PlainParagraph
        | ContentType::LongSingleParagraph
        | ContentType::ShortDisconnectedParagraph => "paragraph",
        ContentType::BulletNumberedList => "list",
        ContentType::CodeBlock => "code_block",
        ContentType::Table => "table",
        _ => "other",
    }
}

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_section_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
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
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut current: Option<SectionBody> = None;
    let mut chunk_index = 0usize;

    for block in &blocks {
        if block.content_type == ContentType::HeadingSection {
            if let Some(body) = current.take() {
                flush_section(&mut result, body, &mut chunk_index, total);
            }
            let level = heading_level_txt(&block.content);
            let text = extract_heading_text(&block.content);
            update_heading_stack(&mut heading_stack, level, text.clone());

            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection,
                content: text.clone(),
                metadata: json!({
                    "section_heading":  text.clone(),
                    "section_level":    level,
                    "heading_path":     heading_path_strings(&heading_stack),
                    "paragraph_count":  0,
                    "block_types":      ["heading"],
                    "char_count":       text.len(),
                    "chunk_index":      chunk_index,
                    "document_metadata": { "source_type": "txt", "total_input_blocks": total }
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
            let bts = block_type_str(block.content_type);
            let a = current.get_or_insert_with(|| SectionBody {
                parts: Vec::new(),
                section_heading: "Preamble".to_string(),
                section_level: current_section_level(&heading_stack),
                heading_path: heading_path_strings(&heading_stack),
            });
            if a.char_count() + block.content.len() + 2 > MAX_SECTION_CHARS && !a.parts.is_empty() {
                let next_heading = a.section_heading.clone();
                let next_level = a.section_level;
                let next_path = a.heading_path.clone();
                let body = current.take().unwrap();
                flush_section(&mut result, body, &mut chunk_index, total);
                current = Some(SectionBody {
                    parts: Vec::new(),
                    section_heading: next_heading,
                    section_level: next_level,
                    heading_path: next_path,
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
