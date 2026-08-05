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
    parse_markdown_blocks, split_at_paragraph_boundary_spanned, strip_block_content, update_heading_stack,
    ChunkRecordInput, ContentType, MdBlockType, SpannedRecord,
};

const MAX_SECTION_CHARS: usize = 2000;

// ── Internal record ───────────────────────────────────────────────────────────

struct SectionBody {
    parts: Vec<(String, &'static str)>, // (content, block_type_str)
    /// The source block of each entry in `parts`, positionally.
    part_blocks: Vec<usize>,
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
    result: &mut Vec<SpannedRecord>,
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
    // Each part of an oversized section reports the blocks *it* covers, not the
    // whole section's: a chunk that claims records it does not contain is worse
    // than one that claims none.
    let tagged: Vec<(String, usize)> = body
        .parts
        .iter()
        .zip(body.part_blocks.iter())
        .map(|((content, _), block)| (content.clone(), *block))
        .collect();
    let parts = split_at_paragraph_boundary_spanned(&tagged, MAX_SECTION_CHARS);
    let part_count = parts.len();
    for (i, (content, span)) in parts.into_iter().enumerate() {
        if content.is_empty() {
            continue;
        }
        result.push(SpannedRecord::spanning(ChunkRecordInput {
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
        }, span));
        *chunk_index += 1;
    }
}

// ── Core algorithm ────────────────────────────────────────────────────────────

pub fn build_section_chunks(bytes: &[u8]) -> Result<Vec<SpannedRecord>, String> {
    let text = crate::text_encoding::decode_utf8_document(bytes);
    if text.trim().is_empty() {
        return Err("Markdown file is empty".to_string());
    }

    let blocks = parse_markdown_blocks(&text);
    let total_input_blocks = blocks.len();
    let mut result: Vec<SpannedRecord> = Vec::new();
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
                result.push(SpannedRecord::at(ChunkRecordInput {
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
                }, block.index));
                chunk_index += 1;

                // Open a fresh body for content that follows.
                current = Some(SectionBody {
                    parts: Vec::new(),
                    section_heading: Some(text),
                    section_level: level,
                    heading_path: heading_path_strings(&heading_stack),
                        part_blocks: Vec::new(),
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
                            part_blocks: Vec::new(),
                        });
                    }
                    {
                        let body = current.as_mut().unwrap();
                        body.parts.push((clean, "paragraph"));
                        body.part_blocks.push(block.index);
                    }
                } else {
                    // Content before any heading — preamble section.
                    current = Some(SectionBody {
                        parts: vec![(clean, "paragraph")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                        part_blocks: vec![block.index],
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
                    body.part_blocks.push(block.index);
                } else {
                    current = Some(SectionBody {
                        parts: vec![(clean, "list")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                        part_blocks: vec![block.index],
                    });
                }
            }

            MdBlockType::Code => {
                // Code blocks are inlined into the section body, not emitted standalone.
                if let Some(ref mut body) = current {
                    body.parts.push((block.content, "code_block"));
                    body.part_blocks.push(block.index);
                } else {
                    current = Some(SectionBody {
                        parts: vec![(block.content, "code_block")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                        part_blocks: vec![block.index],
                    });
                }
            }

            MdBlockType::Table => {
                if let Some(ref mut body) = current {
                    body.parts.push((block.content, "table"));
                    body.part_blocks.push(block.index);
                } else {
                    current = Some(SectionBody {
                        parts: vec![(block.content, "table")],
                        section_heading: None,
                        section_level: current_section_level(&heading_stack),
                        heading_path: heading_path_strings(&heading_stack),
                        part_blocks: vec![block.index],
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

