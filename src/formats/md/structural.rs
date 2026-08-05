use serde_json::{json, Value};

use super::common::{
    classify_prose, extract_heading_text, heading_level, parse_markdown_blocks, split_at_sentences,
    extend_span, strip_block_content, union_span, BlockSpan, ChunkRecordInput, ContentType, MdBlockType,
    SpannedRecord, MAX_CHUNK_CHARS, MIN_CHUNK_CHARS,
};

// ── Prose helpers ─────────────────────────────────────────────────────────────

fn flush_prose(
    chunks: &mut Vec<SpannedRecord>,
    heading: &Option<String>,
    section_level: u8,
    parts: &mut Vec<String>,
    len: &mut usize,
    span: &mut Option<BlockSpan>,
) {
    if parts.is_empty() {
        return;
    }
    let content = parts.join("\n").trim().to_string();
    parts.clear();
    *len = 0;
    let blocks = span.take();
    if content.is_empty() {
        return;
    }
    chunks.push(SpannedRecord::spanning(
        ChunkRecordInput {
            content_type: classify_prose(&content),
            content,
            metadata: md_metadata(heading.clone(), section_level),
        },
        blocks,
    ));
}

fn merge_short_prose(chunks: Vec<SpannedRecord>, min_chars: usize) -> Vec<SpannedRecord> {
    let soft_max = MAX_CHUNK_CHARS + min_chars;
    let mut result: Vec<SpannedRecord> = Vec::new();
    for chunk in chunks {
        let SpannedRecord { record: chunk, blocks } = chunk;
        let is_prose = matches!(
            chunk.content_type,
            ContentType::PlainParagraph
                | ContentType::ShortDisconnectedParagraph
                | ContentType::LongSingleParagraph
        );
        if is_prose && chunk.content.len() < min_chars {
            if let Some(previous) = result.last_mut() {
                let prev = &mut previous.record;
                let prev_is_prose = matches!(
                    prev.content_type,
                    ContentType::PlainParagraph
                        | ContentType::ShortDisconnectedParagraph
                        | ContentType::LongSingleParagraph
                );
                if prev_is_prose && prev.content.len() + chunk.content.len() + 1 <= soft_max {
                    prev.content = format!("{}\n{}", prev.content, chunk.content)
                        .trim()
                        .to_string();
                    prev.content_type = classify_prose(&prev.content);
                    // The merged chunk now covers both sources' blocks.
                    previous.blocks = union_span(previous.blocks, blocks);
                    continue;
                }
            }
        }
        result.push(SpannedRecord::spanning(chunk, blocks));
    }
    result
}

fn md_metadata(section_heading: Option<String>, section_level: u8) -> Value {
    json!({
        "section_heading": section_heading,
        "section_level":   section_level,
        "document_metadata": { "source_type": "md" }
    })
}

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_chunks_from_md_bytes(bytes: &[u8]) -> Result<Vec<SpannedRecord>, String> {
    let text = std::str::from_utf8(bytes)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string());

    if text.trim().is_empty() {
        return Err("Markdown file is empty after decoding".to_string());
    }

    let blocks = parse_markdown_blocks(&text);

    let mut chunks: Vec<SpannedRecord> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut cur_section_level: u8 = 0;
    let mut prose_parts: Vec<String> = Vec::new();
    let mut prose_len: usize = 0;
    // The blocks the buffered prose came from, flushed with it.
    let mut prose_span: Option<BlockSpan> = None;

    for block in blocks {
        match block.block_type {
            MdBlockType::Heading => {
                flush_prose(
                    &mut chunks,
                    &current_heading,
                    cur_section_level,
                    &mut prose_parts,
                    &mut prose_len,
                    &mut prose_span,
                );
                let heading_text = extract_heading_text(&block.content);
                let level = heading_level(&block.content);
                current_heading = Some(heading_text.clone());
                cur_section_level = level;
                chunks.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::HeadingSection,
                    content: heading_text,
                    metadata: json!({
                        "section_heading": serde_json::Value::Null,
                        "section_level": level,
                        "document_metadata": { "source_type": "md" }
                    }),
                }, block.index));
            }
            MdBlockType::Code => {
                flush_prose(
                    &mut chunks,
                    &current_heading,
                    cur_section_level,
                    &mut prose_parts,
                    &mut prose_len,
                    &mut prose_span,
                );
                chunks.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::CodeBlock,
                    content: block.content,
                    metadata: md_metadata(current_heading.clone(), cur_section_level),
                }, block.index));
            }
            MdBlockType::Table => {
                flush_prose(
                    &mut chunks,
                    &current_heading,
                    cur_section_level,
                    &mut prose_parts,
                    &mut prose_len,
                    &mut prose_span,
                );
                chunks.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::Table,
                    content: block.content,
                    metadata: md_metadata(current_heading.clone(), cur_section_level),
                }, block.index));
            }
            MdBlockType::List => {
                flush_prose(
                    &mut chunks,
                    &current_heading,
                    cur_section_level,
                    &mut prose_parts,
                    &mut prose_len,
                    &mut prose_span,
                );
                let clean = strip_block_content(&block.content, true);
                if !clean.is_empty() {
                    chunks.push(SpannedRecord::at(ChunkRecordInput {
                        content_type: ContentType::BulletNumberedList,
                        content: clean,
                        metadata: md_metadata(current_heading.clone(), cur_section_level),
                    }, block.index));
                }
            }
            MdBlockType::Paragraph => {
                let clean = strip_block_content(&block.content, false);
                if clean.is_empty() {
                    continue;
                }
                let sub_blocks = if clean.len() > MAX_CHUNK_CHARS {
                    split_at_sentences(&clean, MAX_CHUNK_CHARS)
                } else {
                    vec![clean]
                };
                for sub in sub_blocks {
                    let add = sub.len() + 1;
                    if prose_len + add > MAX_CHUNK_CHARS && !prose_parts.is_empty() {
                        flush_prose(
                            &mut chunks,
                            &current_heading,
                            cur_section_level,
                            &mut prose_parts,
                            &mut prose_len,
                            &mut prose_span,
                        );
                    }
                    prose_len += add;
                    prose_parts.push(sub);
                    extend_span(&mut prose_span, block.index);
                }
            }
        }
    }

    flush_prose(
        &mut chunks,
        &current_heading,
        cur_section_level,
        &mut prose_parts,
        &mut prose_len,
        &mut prose_span,
    );
    let chunks = merge_short_prose(chunks, MIN_CHUNK_CHARS);

    if chunks.is_empty() {
        return Err("No chunks generated from Markdown document".to_string());
    }
    Ok(chunks)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────

