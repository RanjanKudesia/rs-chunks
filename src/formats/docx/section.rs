use serde_json::{json, Value};

use super::common::{
    image_hash_name, image_placeholder, parse_docx_blocks, DocxBlock,
    DocxBlockKind,
};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const MAX_SECTION_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockType {
    Paragraph,
    BulletList,
    Table,
    Image,
}

#[derive(Debug, Clone)]
struct DocumentBlock {
    block_type: BlockType,
    text: String,
    heading_level: Option<u32>,
    image_rid: Option<String>,
}

#[derive(Debug, Clone)]
struct SectionState {
    heading: String,
    level: u32,
    path: Vec<String>,
    lines: Vec<String>,
    /// Position of this section's heading in the document.
    ///
    /// Sections are *closed* in the reverse of the order they open — a nested
    /// subsection pops before its parent, and the outermost section not until
    /// EOF. Emitting at close time therefore returns chunks in an order that
    /// has nothing to do with reading order. Recording where each section began
    /// lets the emission be sorted back (TECH_DEBT #1).
    order: usize,
}

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content: String,
    metadata: Value,
}

fn build_section_chunks_with_images(
    blocks: Vec<DocumentBlock>,
    image_rids_map: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<(String, String, serde_json::Value)> {
    let mut result: Vec<(String, String, serde_json::Value)> = Vec::new();
    let section_chunks = build_section_chunks(blocks.clone());

    let mut image_results: Vec<(String, String, serde_json::Value)> = Vec::new();
    for block in &blocks {
        if block.block_type != BlockType::Image {
            continue;
        }
        if let Some(rid) = &block.image_rid {
            if let Some(zip_path) = image_rids_map.get(rid) {
                if let Ok(mut entry) = archive.by_name(zip_path) {
                    let mut bytes = Vec::new();
                    if entry.read_to_end(&mut bytes).is_ok() {
                        if let Some(hash_name) = image_hash_name(&bytes, zip_path) {
                            if !image_out.iter().any(|(n, _)| n == &hash_name) {
                                image_out.push((hash_name.clone(), bytes));
                            }
                            let alt = block
                                .text
                                .strip_prefix("[Image: ")
                                .and_then(|s| s.strip_suffix(']'))
                                .unwrap_or("");
                            image_results.push((
                                "image".to_string(),
                                hash_name.clone(),
                                json!({ "image_name": hash_name, "alt_text": alt }),
                            ));
                        }
                    }
                }
            }
        }
    }

    result.extend(image_results);
    for chunk in section_chunks {
        result.push(("section".to_string(), chunk.content, chunk.metadata));
    }
    result
}

fn lower_blocks(raw: Vec<DocxBlock>) -> Vec<DocumentBlock> {
    let mut out: Vec<DocumentBlock> = Vec::with_capacity(raw.len());

    for block in raw {
        match block.kind {
            DocxBlockKind::Table => {
                let table_text = block.text.trim().to_string();
                if !table_text.is_empty() {
                    out.push(DocumentBlock {
                        block_type: BlockType::Table,
                        text: table_text,
                        heading_level: None,
                        image_rid: None,
                    });
                }
            }
            DocxBlockKind::Paragraph => {
                let text = block.text.trim().to_string();
                let has_text = !text.is_empty();

                if !has_text && !block.has_drawing {
                    continue;
                }

                let heading_level =
                    parse_heading_level(block.heading_style.as_deref(), block.outline_level);

                let normalized = if has_text {
                    text
                } else {
                    image_placeholder(block.image_alt.as_deref())
                };

                let block_type = if heading_level.is_some() {
                    BlockType::Paragraph
                } else if block.is_list {
                    BlockType::BulletList
                } else if block.has_drawing {
                    BlockType::Image
                } else {
                    BlockType::Paragraph
                };

                out.push(DocumentBlock {
                    block_type,
                    text: normalized,
                    heading_level,
                    image_rid: if block.has_drawing {
                        block.image_rid.clone()
                    } else {
                        None
                    },
                });
            }
        }
    }

    out
}

fn parse_heading_level(style_val: Option<&str>, outline_level: Option<u32>) -> Option<u32> {
    if let Some(style) = style_val {
        let style_trimmed = style.trim();
        let style_lc = style_trimmed.to_ascii_lowercase();

        if style_lc.starts_with("heading") {
            let level_part = style_trimmed
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .collect::<String>();

            if level_part.is_empty() {
                // Malformed heading style data is treated as non-heading.
                return None;
            }

            if let Ok(level) = level_part.parse::<u32>() {
                if level > 0 {
                    return Some(level);
                }
            }

            // Malformed heading style data is treated as non-heading.
            return None;
        }

        // Not a heading style: fallback to outline level when available.
        if let Some(outline) = outline_level {
            return Some(outline.saturating_add(1));
        }
        return None;
    }

    outline_level.map(|v| v.saturating_add(1))
}
fn build_section_chunks(blocks: Vec<DocumentBlock>) -> Vec<ChunkRecordInput> {
    let mut chunks = Vec::new();
    let mut preamble_lines: Vec<String> = Vec::new();
    let mut sections: Vec<SectionState> = Vec::new();
    let mut seen_heading = false;
    // Sections are collected as they close and sorted back into document order
    // before emission — see `SectionState::order`.
    let mut closed: Vec<SectionState> = Vec::new();
    let mut next_order = 0usize;

    for block in blocks {
        if let Some(level) = block.heading_level {
            seen_heading = true;

            while let Some(last) = sections.last() {
                if last.level >= level {
                    closed.push(sections.pop().expect("section stack not empty"));
                } else {
                    break;
                }
            }

            let heading = block.text.clone();
            let mut path = if let Some(parent) = sections.last() {
                parent.path.clone()
            } else {
                Vec::new()
            };
            path.push(heading.clone());

            sections.push(SectionState {
                heading: heading.clone(),
                level,
                path,
                lines: vec![heading],
                order: next_order,
            });
            next_order += 1;
            continue;
        }

        let line = normalize_block_line(&block);
        if line.is_empty() {
            continue;
        }

        if let Some(current) = sections.last_mut() {
            current.lines.push(line);
        } else if !seen_heading {
            preamble_lines.push(line);
        }
    }

    // Anything still open at EOF closes now; it is already ordered by `order`.
    closed.extend(sections);
    closed.sort_by_key(|section| section.order);

    // The preamble precedes every heading in the document, so it is emitted
    // first — previously it was emitted after every section that had closed
    // during the loop, which put the document's opening text near the end.
    if !preamble_lines.is_empty() {
        let preamble_path = vec!["Preamble".to_string()];
        emit_section_chunks(&mut chunks, "Preamble", 0, &preamble_path, &preamble_lines);
    }

    for section in closed {
        emit_section_chunks(
            &mut chunks,
            &section.heading,
            section.level,
            &section.path,
            &section.lines,
        );
    }

    chunks
}

fn normalize_block_line(block: &DocumentBlock) -> String {
    match block.block_type {
        BlockType::Table | BlockType::BulletList => block.text.trim().to_string(),
        BlockType::Paragraph | BlockType::Image => block.text.trim().to_string(),
    }
}

fn emit_section_chunks(
    out: &mut Vec<ChunkRecordInput>,
    heading: &str,
    level: u32,
    path: &[String],
    lines: &[String],
) {
    let full_content = lines.join("\n").trim().to_string();
    if full_content.is_empty() {
        return;
    }

    let splits = split_text_by_max_chars(&full_content, MAX_SECTION_CHARS);
    let total = splits.len();

    for (idx, content) in splits.into_iter().enumerate() {
        let mut metadata = json!({
            "section_heading": heading,
            "section_level": level,
            "section_heading_level": level,
            "heading_path": path.join(" > "),
            "document_metadata": {
                "source_type": "docx"
            }
        });

        if total > 1 {
            if let Some(meta_obj) = metadata.as_object_mut() {
                meta_obj.insert("chunk_index".to_string(), json!(idx + 1));
                meta_obj.insert("total_chunks".to_string(), json!(total));
            }
        }

        out.push(ChunkRecordInput { content, metadata });
    }
}

fn split_text_by_max_chars(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }

        if line.len() > max_chars {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
                current.clear();
            }

            let mut start = 0usize;
            while start < line.len() {
                let mut end = (start + max_chars).min(line.len());
                end = crate::shared::floor_char_boundary(line, end);
                if end <= start {
                    // max_chars smaller than a single char; advance one whole char.
                    end = crate::shared::floor_char_boundary(line, start + 4).max(start + 1).min(line.len());
                }
                out.push(line[start..end].trim().to_string());
                start = end;
            }
            continue;
        }

        let candidate = if current.is_empty() {
            line.to_string()
        } else {
            format!("{}\n{}", current, line)
        };

        if candidate.len() > max_chars {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
            }
            current = line.to_string();
        } else {
            current = candidate;
        }
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    if out.is_empty() {
        vec![text.to_string()]
    } else {
        out
    }
}


pub(super) fn chunk(bytes: &[u8]) -> Result<Vec<crate::chunk::Chunk>, String> {
    let raw_blocks = parse_docx_blocks(bytes)?;
    let blocks = lower_blocks(raw_blocks);
    Ok(build_section_chunks(blocks)
        .into_iter()
        .map(|c| crate::chunk::Chunk::new(c.content, "section", c.metadata))
        .collect())
}

pub(super) fn chunk_with_images(bytes: &[u8]) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let blocks = lower_blocks(parse_docx_blocks(bytes)?);
    let mut image_out = Vec::new();
    let combined = build_section_chunks_with_images(blocks, &image_rids_map, &mut archive, &mut image_out);
    Ok((combined.into_iter().map(|(ct, c, m)| crate::chunk::Chunk::new(c, ct, m)).collect(), image_out))
}
