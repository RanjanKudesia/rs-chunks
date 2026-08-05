use serde_json::{json, Value};

use super::common::{
    collapse_whitespace, docx_heading_level, image_hash_name, parse_docx_blocks, DocxBlock, DocxBlockKind,
};
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const MAX_CHUNK_CHARS: usize = 1500;
const SHORT_PARAGRAPH_CHARS: usize = 80;

const REFERENCE_STARTS: [&str; 8] = [
    "this", "it", "they", "these", "that", "those", "its", "their",
];

const TRANSITION_STARTS: [&str; 12] = [
    "however",
    "nevertheless",
    "in contrast",
    "on the other hand",
    "meanwhile",
    "conversely",
    "that said",
    "in summary",
    "to conclude",
    "therefore",
    "thus",
    "hence",
];

use crate::shared::STOPWORDS;

#[derive(Debug, Clone)]
struct SemanticParagraph {
    text: String,
    is_heading: bool,
    heading_level: Option<u32>,
    is_image: bool,
    image_rid: Option<String>,
    /// Word marked this paragraph as a list item (`<w:numPr>`).
    ///
    /// Kept because a run of list items is **one** semantic unit. Without it
    /// the short-paragraph rules below cap a chunk at three bullets and a
    /// six-item outline comes back as three 30-character chunks (TECH_DEBT #3).
    is_list: bool,
}

#[derive(Debug, Clone)]
struct SemanticChunk {
    paragraphs: Vec<String>,
    merge_reason: &'static str,
    section_heading: Option<String>,
    section_heading_level: Option<u32>,
}

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content: String,
    metadata: Value,
}

fn build_semantic_chunks_with_images(
    paragraphs: Vec<SemanticParagraph>,
    image_rids_map: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<(String, String, serde_json::Value)> {
    let mut result: Vec<(String, String, serde_json::Value)> = Vec::new();

    for para in &paragraphs {
        if !para.is_image {
            continue;
        }
        if let Some(rid) = &para.image_rid {
            if let Some(zip_path) = image_rids_map.get(rid) {
                if let Ok(mut entry) = archive.by_name(zip_path) {
                    let mut bytes = Vec::new();
                    if entry.read_to_end(&mut bytes).is_ok() {
                        if let Some(hash_name) = image_hash_name(&bytes, zip_path) {
                            if !image_out.iter().any(|(n, _)| n == &hash_name) {
                                image_out.push((hash_name.clone(), bytes));
                            }
                            let alt = para
                                .text
                                .strip_prefix("[Image: ")
                                .and_then(|s| s.strip_suffix(']'))
                                .unwrap_or("");
                            result.push((
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

    let text_chunks = build_semantic_chunks(paragraphs);
    for chunk in text_chunks {
        result.push(("semantic".to_string(), chunk.content, chunk.metadata));
    }

    result
}

fn lower_blocks_to_paragraphs(raw: Vec<DocxBlock>) -> Vec<SemanticParagraph> {
    let mut out: Vec<SemanticParagraph> = Vec::with_capacity(raw.len());

    for block in raw {
        match block.kind {
            DocxBlockKind::Table => {
                let table_text = block.text.trim().to_string();
                if !table_text.is_empty() {
                    out.push(SemanticParagraph {
                        text: table_text,
                        is_heading: false,
                        heading_level: None,
                        is_image: false,
                        image_rid: None,
                        is_list: false,
                    });
                }
            }
            DocxBlockKind::Paragraph => {
                let heading_level =
                    docx_heading_level(block.heading_style.as_deref(), block.outline_level);
                let is_heading = heading_level.is_some();
                let text = block.text.trim().to_string();
                if !text.is_empty() {
                    let listed = if block.is_list {
                        format!("- {text}")
                    } else {
                        text
                    };
                    // A paragraph holding text *and* an image keeps both (#83).
                    let normalized = super::common::text_with_image_marker(
                        listed,
                        block.has_drawing,
                        block.image_alt.as_deref(),
                    );
                    out.push(SemanticParagraph {
                        text: normalized,
                        is_heading,
                        heading_level,
                        is_image: block.has_drawing,
                        image_rid: block.image_rid.clone(),
                        is_list: block.is_list,
                    });
                } else if block.has_drawing {
                    out.push(SemanticParagraph {
                        text: super::common::image_placeholder(block.image_alt.as_deref()),
                        is_heading: false,
                        heading_level: None,
                        is_image: true,
                        image_rid: block.image_rid.clone(),
                        is_list: false,
                    });
                }
            }
        }
    }

    out
}

fn build_semantic_chunks(paragraphs: Vec<SemanticParagraph>) -> Vec<ChunkRecordInput> {
    let cleaned: Vec<SemanticParagraph> = paragraphs
        .into_iter()
        .map(|p| SemanticParagraph {
            text: collapse_whitespace(&p.text),
            is_heading: p.is_heading,
            heading_level: p.heading_level,
            is_image: p.is_image,
            image_rid: p.image_rid,
            is_list: p.is_list,
        })
        .filter(|p| !p.text.is_empty())
        .collect();

    if cleaned.is_empty() {
        return Vec::new();
    }

    let semantic_chunks = propagate_section_headings(
        merge_heading_singletons(group_semantic_chunks(cleaned)),
    );
    semantic_chunks
        .into_iter()
        .map(|chunk| {
            let content = chunk.paragraphs.join("\n\n");
            ChunkRecordInput {
                content,
                metadata: json!({
                    "section_heading": chunk.section_heading,
                    "section_heading_level": chunk.section_heading_level,
                    "paragraph_count": chunk.paragraphs.len(),
                    "merge_reason": chunk.merge_reason,
                    "document_metadata": {
                        "source_type": "docx"
                    }
                }),
            }
        })
        .collect()
}

/// Fold a lone heading into the body chunk that follows it.
///
/// A heading is held back until the next chunk arrives, because it only reads
/// as a heading once there is a body to head. But holding it back is not the
/// same as discarding it: if no body ever arrives — the document ends on a
/// heading, or two headings run together, or the whole document is short
/// paragraphs — the held-back text must still be emitted on its own.
///
/// It previously was not, and `is_heading_singleton` treats *any* paragraph
/// under 30 characters as a heading. So a document made only of short lines had
/// every line held back and then dropped, and semantic mode returned nothing at
/// all while every other mode returned the text (poi_chartex.docx,
/// poi_saut_page.docx).
fn merge_heading_singletons(chunks: Vec<SemanticChunk>) -> Vec<SemanticChunk> {
    let mut merged: Vec<SemanticChunk> = Vec::new();
    let mut pending: Option<SemanticChunk> = None;

    for mut chunk in chunks {
        if is_heading_singleton(&chunk) {
            // A second heading in a row means the first one never found a body.
            // Emit it as its own chunk rather than letting it fall on the floor.
            if let Some(orphan) = pending.replace(chunk) {
                merged.push(orphan);
            }
            continue;
        }

        if let Some(held) = pending.take() {
            if has_actual_body_content(&chunk) {
                let heading = held.paragraphs.into_iter().next().unwrap_or_default();
                if chunk.section_heading.is_none() {
                    chunk.section_heading = Some(heading.clone());
                }
                if chunk.section_heading_level.is_none() {
                    chunk.section_heading_level = held.section_heading_level;
                }
                let mut paragraphs = vec![heading];
                paragraphs.extend(chunk.paragraphs);
                chunk.paragraphs = paragraphs;
                chunk.merge_reason = "heading_merge";
            } else {
                // Nothing substantial to attach to — keep both, separately.
                merged.push(held);
            }
        }

        merged.push(chunk);
    }

    // The document ended on a heading.
    if let Some(orphan) = pending {
        merged.push(orphan);
    }

    merged
}

fn propagate_section_headings(mut chunks: Vec<SemanticChunk>) -> Vec<SemanticChunk> {
    let mut last_heading: Option<String> = None;
    let mut last_level: Option<u32> = None;
    for chunk in &mut chunks {
        if chunk.section_heading.is_some() {
            last_heading = chunk.section_heading.clone();
            last_level = chunk.section_heading_level;
        } else if last_heading.is_some() {
            chunk.section_heading = last_heading.clone();
            chunk.section_heading_level = last_level;
        }
    }
    chunks
}

fn is_heading_singleton(chunk: &SemanticChunk) -> bool {
    chunk.paragraphs.len() == 1
        && (chunk.merge_reason == "docx_heading" || chunk.paragraphs[0].len() < 30)
}

fn has_actual_body_content(chunk: &SemanticChunk) -> bool {
    if is_heading_singleton(chunk) {
        return false;
    }

    if chunk.paragraphs.len() == 1 {
        return chunk.paragraphs[0].len() > 50;
    }

    let first = &chunk.paragraphs[0];
    if first.len() < 30 {
        let body_len = chunk.paragraphs[1..].join("\n\n").len();
        return body_len > 50;
    }

    chunk.paragraphs.join("\n\n").len() > 50
}

fn group_semantic_chunks(paragraphs: Vec<SemanticParagraph>) -> Vec<SemanticChunk> {
    let mut chunks = Vec::new();
    let first = &paragraphs[0];
    let mut current = SemanticChunk {
        paragraphs: vec![first.text.clone()],
        merge_reason: if first.is_heading {
            "docx_heading"
        } else {
            "keyword_overlap"
        },
        section_heading: None,
        section_heading_level: if first.is_heading {
            first.heading_level
        } else {
            None
        },
    };

    let mut force_merge_next = false;
    // Whether the paragraph most recently added to `current` was a list item,
    // so a run of them can be held together (see the list_continuation rule).
    let mut current_ends_in_list = paragraphs[0].is_list;

    for sp in paragraphs.iter().skip(1) {
        let para = &sp.text;
        // By the end of this iteration `current` always ends with `sp` —
        // every branch below either appends it or starts a new chunk from it —
        // so the flag can be updated up front, keeping the previous value for
        // the list-continuation test.
        let prev_ends_in_list = current_ends_in_list;
        current_ends_in_list = sp.is_list;
        // Real DOCX heading paragraph always breaks and becomes its own
        // singleton chunk so `merge_heading_singletons` can attach it to the
        // following body content.
        if sp.is_heading {
            chunks.push(current);
            current = SemanticChunk {
                paragraphs: vec![para.clone()],
                merge_reason: "docx_heading",
                section_heading: None,
                section_heading_level: sp.heading_level,
            };
            force_merge_next = false;
            continue;
        }

        let para_is_short = is_short_paragraph(para);
        let mut merge = false;
        let mut merge_reason = current.merge_reason;
        let mut pending_break_reason: Option<&'static str> = None;

        // A run of list items is one semantic unit. This is checked before the
        // short-paragraph rules because those cap a chunk at three consecutive
        // short paragraphs — correct for prose, but it chops a bulleted list
        // into arbitrary thirds (TECH_DEBT #3). `MAX_CHUNK_CHARS` still applies
        // below, so a genuinely huge list is still split on size.
        if sp.is_list && prev_ends_in_list {
            merge = true;
            merge_reason = "list_continuation";
        } else if starts_with_reference_pronoun(para) {
            merge = true;
            merge_reason = "reference_continuity";
        } else if starts_with_transition_keyword(para) {
            pending_break_reason = Some("transition_break");
        } else if keyword_overlap_count(&current.paragraphs, para) >= 2 {
            merge = true;
            merge_reason = "keyword_overlap";
        } else if force_merge_next && can_short_merge(&current.paragraphs, para, para_is_short) {
            merge = true;
            merge_reason = "short_paragraph";
        }

        if merge {
            // When the first merge into a heading-only chunk happens, capture the
            // heading text as section_heading before the merge_reason gets overwritten.
            if current.paragraphs.len() == 1
                && current.merge_reason == "docx_heading"
                && current.section_heading.is_none()
            {
                current.section_heading = Some(current.paragraphs[0].clone());
            }
            let merged_len = chunk_content_len(&current.paragraphs) + 2 + para.len();
            if merged_len > MAX_CHUNK_CHARS {
                current.merge_reason = "size_limit";
                chunks.push(current);
                current = SemanticChunk {
                    paragraphs: vec![para.clone()],
                    merge_reason: "size_limit",
                    section_heading: None,
                    section_heading_level: None,
                };
                force_merge_next = para_is_short && para.contains(' ');
                continue;
            }

            current.paragraphs.push(para.clone());
            current.merge_reason = merge_reason;
            force_merge_next = para_is_short && para.contains(' ');
            continue;
        }

        if let Some(reason) = pending_break_reason {
            current.merge_reason = reason;
            chunks.push(current);
            current = SemanticChunk {
                paragraphs: vec![para.clone()],
                merge_reason: reason,
                section_heading: None,
                section_heading_level: None,
            };
            force_merge_next = para_is_short && para.contains(' ');
            continue;
        }

        if para_is_short {
            chunks.push(current);
            current = SemanticChunk {
                paragraphs: vec![para.clone()],
                merge_reason: "short_paragraph",
                section_heading: None,
                section_heading_level: None,
            };
            force_merge_next = para.contains(' ');
            continue;
        }

        chunks.push(current);
        current = SemanticChunk {
            paragraphs: vec![para.clone()],
            merge_reason: "keyword_overlap",
            section_heading: None,
            section_heading_level: None,
        };
        force_merge_next = false;
    }

    chunks.push(current);
    chunks
}

fn is_short_paragraph(text: &str) -> bool {
    text.len() < SHORT_PARAGRAPH_CHARS
}

fn can_short_merge(
    current_paragraphs: &[String],
    next_paragraph: &str,
    next_is_short: bool,
) -> bool {
    if !next_paragraph.contains(' ') {
        return false;
    }

    if current_paragraphs.len() >= 3 && next_is_short {
        return false;
    }

    if trailing_short_paragraphs(current_paragraphs) >= 3 {
        return false;
    }

    true
}

fn trailing_short_paragraphs(paragraphs: &[String]) -> usize {
    paragraphs
        .iter()
        .rev()
        .take_while(|paragraph| is_short_paragraph(paragraph))
        .count()
}

fn starts_with_reference_pronoun(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    REFERENCE_STARTS.iter().any(|prefix| {
        lower == *prefix
            || lower
                .strip_prefix(prefix)
                .map(|rest| rest.starts_with(' ') || rest.starts_with(',') || rest.starts_with(':'))
                .unwrap_or(false)
    })
}

fn starts_with_transition_keyword(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    TRANSITION_STARTS.iter().any(|prefix| {
        lower == *prefix
            || lower
                .strip_prefix(prefix)
                .map(|rest| rest.starts_with(' ') || rest.starts_with(',') || rest.starts_with(':'))
                .unwrap_or(false)
    })
}

fn keyword_overlap_count(current_chunk: &[String], next_paragraph: &str) -> usize {
    let current_words = extract_keywords(&current_chunk.join(" "));
    let next_words = extract_keywords(next_paragraph);
    current_words.intersection(&next_words).count()
}

fn extract_keywords(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_ascii_alphabetic())
        .map(|word| word.to_ascii_lowercase())
        .filter(|word| word.len() > 4)
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect()
}

fn chunk_content_len(paragraphs: &[String]) -> usize {
    if paragraphs.is_empty() {
        return 0;
    }
    paragraphs.iter().map(|p| p.len()).sum::<usize>() + ((paragraphs.len() - 1) * 2)
}


pub(super) fn chunk(bytes: &[u8]) -> Result<Vec<crate::chunk::Chunk>, String> {
    let raw_blocks = parse_docx_blocks(bytes)?;
    let paragraphs = lower_blocks_to_paragraphs(raw_blocks);
    Ok(build_semantic_chunks(paragraphs)
        .into_iter()
        .map(|c| crate::chunk::Chunk::new(c.content, "semantic", c.metadata))
        .collect())
}

pub(super) fn chunk_with_images(bytes: &[u8]) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let paragraphs = lower_blocks_to_paragraphs(parse_docx_blocks(bytes)?);
    let mut image_out = Vec::new();
    let combined = build_semantic_chunks_with_images(paragraphs, &image_rids_map, &mut archive, &mut image_out);
    Ok((combined.into_iter().map(|(ct, c, m)| crate::chunk::Chunk::new(c, ct, m)).collect(), image_out))
}
