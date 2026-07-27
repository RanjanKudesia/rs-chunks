
use super::common::{
    classify_prose, html_metadata, parse_html_blocks, remove_comments, split_at_sentences,
    ChunkRecordInput, ContentType, HtmlBlockType, MAX_CHUNK_CHARS, MIN_CHUNK_CHARS,
};

// ── Prose helpers ─────────────────────────────────────────────────────────────

fn flush_prose(
    chunks: &mut Vec<ChunkRecordInput>,
    heading: &Option<String>,
    parts: &mut Vec<String>,
    len: &mut usize,
) {
    if parts.is_empty() { return; }
    let content = parts.join("\n").trim().to_string();
    parts.clear(); *len = 0;
    if content.is_empty() { return; }
    chunks.push(ChunkRecordInput {
        content_type: classify_prose(&content),
        content,
        metadata: html_metadata(heading.clone()),
    });
}

fn merge_short_prose(chunks: Vec<ChunkRecordInput>, min_chars: usize) -> Vec<ChunkRecordInput> {
    let soft_max = MAX_CHUNK_CHARS + min_chars;
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    for chunk in chunks {
        let is_prose = matches!(
            chunk.content_type,
            ContentType::PlainParagraph | ContentType::ShortDisconnectedParagraph | ContentType::LongSingleParagraph
        );
        if is_prose && chunk.content.len() < min_chars {
            if let Some(prev) = result.last_mut() {
                let prev_prose = matches!(
                    prev.content_type,
                    ContentType::PlainParagraph | ContentType::ShortDisconnectedParagraph | ContentType::LongSingleParagraph
                );
                if prev_prose && prev.content.len() + chunk.content.len() + 1 <= soft_max {
                    prev.content = format!("{}\n{}", prev.content, chunk.content).trim().to_string();
                    prev.content_type = classify_prose(&prev.content);
                    continue;
                }
            }
        }
        result.push(chunk);
    }
    result
}

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_chunks_from_html_bytes(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let text = std::str::from_utf8(bytes)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string());
    if text.trim().is_empty() { return Err("HTML file is empty after decoding".to_string()); }

    let blocks = parse_html_blocks(&remove_comments(&text));
    let mut chunks = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut prose_parts: Vec<String> = Vec::new();
    let mut prose_len = 0usize;

    for block in blocks {
        match block.block_type {
            HtmlBlockType::Heading => {
                flush_prose(&mut chunks, &current_heading, &mut prose_parts, &mut prose_len);
                current_heading = Some(block.content.clone());
                chunks.push(ChunkRecordInput {
                    content_type: ContentType::HeadingSection,
                    content: block.content,
                    metadata: html_metadata(None),
                });
            }
            HtmlBlockType::Code => {
                flush_prose(&mut chunks, &current_heading, &mut prose_parts, &mut prose_len);
                chunks.push(ChunkRecordInput {
                    content_type: ContentType::CodeBlock,
                    content: block.content,
                    metadata: html_metadata(current_heading.clone()),
                });
            }
            HtmlBlockType::Table => {
                flush_prose(&mut chunks, &current_heading, &mut prose_parts, &mut prose_len);
                chunks.push(ChunkRecordInput {
                    content_type: ContentType::Table,
                    content: block.content,
                    metadata: html_metadata(current_heading.clone()),
                });
            }
            HtmlBlockType::List => {
                flush_prose(&mut chunks, &current_heading, &mut prose_parts, &mut prose_len);
                if !block.content.is_empty() {
                    chunks.push(ChunkRecordInput {
                        content_type: ContentType::BulletNumberedList,
                        content: block.content,
                        metadata: html_metadata(current_heading.clone()),
                    });
                }
            }
            HtmlBlockType::Paragraph => {
                if block.content.is_empty() { continue; }
                let parts = if block.content.len() > MAX_CHUNK_CHARS {
                    split_at_sentences(&block.content, MAX_CHUNK_CHARS)
                } else {
                    vec![block.content]
                };
                for part in parts {
                    let add = part.len() + 1;
                    if prose_len + add > MAX_CHUNK_CHARS && !prose_parts.is_empty() {
                        flush_prose(&mut chunks, &current_heading, &mut prose_parts, &mut prose_len);
                    }
                    prose_len += add;
                    prose_parts.push(part);
                }
            }
        }
    }
    flush_prose(&mut chunks, &current_heading, &mut prose_parts, &mut prose_len);
    let chunks = merge_short_prose(chunks, MIN_CHUNK_CHARS);
    if chunks.is_empty() { return Err("No chunks generated from HTML document".to_string()); }
    Ok(chunks)
}

