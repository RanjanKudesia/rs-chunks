/// Section chunker for Markdown.
///
/// Groups all content (paragraphs, lists, code blocks, tables) that falls
/// under a heading into a single chunk.  A new heading — at any level —
/// always starts a new section.  Sections that exceed MAX_SECTION_CHARS are
/// split at sentence boundaries.
///
/// Metadata per chunk:
///   section_heading  — the heading text that opened this section
///   section_level    — ATX/setext level (1-6)
///   heading_path     — full ancestor breadcrumb ["H1", "H2", "H3"]
///   paragraph_count  — number of body blocks accumulated
///   block_types      — unique block types present ["paragraph", "list", ...]
///   char_count       — total content characters

use serde_json::json;

use super::common::{
    current_section_level, extract_heading_text, heading_level, heading_path_strings,
    parse_markdown_blocks, split_at_paragraph_boundary, strip_block_content, update_heading_stack,
    ChunkRecordInput, ContentType, MdBlockType,
};

const MAX_SECTION_CHARS: usize = 2000;

// ── Internal record ───────────────────────────────────────────────────────────

struct SectionBody {
    parts: Vec<(String, &'static str)>, // (content, block_type_str)
    section_heading: Option<String>,
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn flush_section(
    result: &mut Vec<ChunkRecordInput>,
    body: SectionBody,
    chunk_index: &mut usize,
    total_input_blocks: usize,
) {
    let joined = body.joined();
    if joined.trim().is_empty() {
        return;
    }
    let block_types = body.block_types();
    let paragraph_count = body.paragraph_count();
    let parts = split_at_paragraph_boundary(&joined, MAX_SECTION_CHARS);
    let part_count = parts.len();
    for (i, content) in parts.into_iter().enumerate() {
        if content.is_empty() {
            continue;
        }
        result.push(ChunkRecordInput {
            content_type: ContentType::Section,
            content: content.clone(),
            metadata: json!({
                "section_heading":      body.section_heading,
                "section_level":        body.section_level,
                "heading_path":         body.heading_path,
                "paragraph_count":      paragraph_count,
                "block_types":          block_types,
                "char_count":           content.len(),
                "split_part":           if part_count > 1 { serde_json::json!(i + 1) } else { serde_json::Value::Null },
                "split_total":          if part_count > 1 { serde_json::json!(part_count) } else { serde_json::Value::Null },
                "chunk_index":          *chunk_index,
                "document_metadata": {
                    "source_type":          "md",
                    "total_input_blocks":   total_input_blocks,
                }
            }),
        });
        *chunk_index += 1;
    }
}

// ── Core algorithm ────────────────────────────────────────────────────────────

pub fn build_section_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let text = std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string());
    if text.trim().is_empty() {
        return Err("Markdown file is empty".to_string());
    }

    let blocks = parse_markdown_blocks(&text);
    let total_input_blocks = blocks.len();
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut current: Option<SectionBody> = None;
    let mut chunk_index = 0usize;

    for block in blocks {
        match block.block_type {
            MdBlockType::Heading => {
                // Flush previous section body.
                if let Some(body) = current.take() {
                    flush_section(&mut result, body, &mut chunk_index, total_input_blocks);
                }

                let level = heading_level(&block.content);
                let text = extract_heading_text(&block.content);
                update_heading_stack(&mut heading_stack, level, text.clone());

                // Emit the heading itself.
                result.push(ChunkRecordInput {
                    content_type: ContentType::HeadingSection,
                    content: text.clone(),
                    metadata: json!({
                        "section_heading":    text,
                        "section_level":      level,
                        "heading_path":       heading_path_strings(&heading_stack),
                        "paragraph_count":    0,
                        "block_types":        ["heading"],
                        "char_count":         text.len(),
                        "chunk_index":        chunk_index,
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                });
                chunk_index += 1;

                // Open a fresh body for content that follows.
                current = Some(SectionBody {
                    parts: Vec::new(),
                    section_heading: Some(text),
                    section_level: level,
                    heading_path: heading_path_strings(&heading_stack),
                });
            }

            MdBlockType::Paragraph => {
                let clean = strip_block_content(&block.content, false);
                if clean.is_empty() {
                    continue;
                }
                // If section would exceed limit, flush and continue under same heading.
                if let Some(ref mut body) = current {
                    if body.char_count() + clean.len() + 2 > MAX_SECTION_CHARS
                        && !body.parts.is_empty()
                    {
                        let finished = current.take().unwrap();
                        let next_heading = finished.section_heading.clone();
                        let next_level = finished.section_level;
                        let next_path = finished.heading_path.clone();
                        flush_section(&mut result, finished, &mut chunk_index, total_input_blocks);
                        current = Some(SectionBody {
                            parts: Vec::new(),
                            section_heading: next_heading,
                            section_level: next_level,
                            heading_path: next_path,
                        });
                    }
                    current.as_mut().unwrap().parts.push((clean, "paragraph"));
                } else {
                    // Content before any heading — preamble section.
                    current = Some(SectionBody {
                        parts: vec![(clean, "paragraph")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                    });
                }
            }

            MdBlockType::List => {
                let clean = strip_block_content(&block.content, true);
                if clean.is_empty() {
                    continue;
                }
                if let Some(ref mut body) = current {
                    body.parts.push((clean, "list"));
                } else {
                    current = Some(SectionBody {
                        parts: vec![(clean, "list")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                    });
                }
            }

            MdBlockType::Code => {
                // Code blocks are inlined into the section body, not emitted standalone.
                if let Some(ref mut body) = current {
                    body.parts.push((block.content, "code_block"));
                } else {
                    current = Some(SectionBody {
                        parts: vec![(block.content, "code_block")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                    });
                }
            }

            MdBlockType::Table => {
                if let Some(ref mut body) = current {
                    body.parts.push((block.content, "table"));
                } else {
                    current = Some(SectionBody {
                        parts: vec![(block.content, "table")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                    });
                }
            }
        }
    }

    if let Some(body) = current.take() {
        flush_section(&mut result, body, &mut chunk_index, total_input_blocks);
    }

    if result.is_empty() {
        return Err("No section chunks generated".to_string());
    }
    Ok(result)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────

