use super::common::{
    classify_prose, extract_heading_text, parse_txt_blocks, split_at_sentences, txt_metadata,
    ChunkRecordInput, ContentType, MAX_CHUNK_CHARS, MIN_CHUNK_CHARS,
};

// ── Prose helpers ─────────────────────────────────────────────────────────────

fn flush_prose(
    chunks: &mut Vec<ChunkRecordInput>,
    heading: &Option<String>,
    parts: &mut Vec<String>,
    len: &mut usize,
) {
    if parts.is_empty() {
        return;
    }
    let content = parts.join("\n").trim().to_string();
    parts.clear();
    *len = 0;
    if content.is_empty() {
        return;
    }
    chunks.push(ChunkRecordInput {
        content_type: classify_prose(&content),
        content,
        metadata: txt_metadata(heading.clone()),
    });
}

fn merge_short_prose(chunks: Vec<ChunkRecordInput>, min_chars: usize) -> Vec<ChunkRecordInput> {
    let soft_max = MAX_CHUNK_CHARS + min_chars;
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    for chunk in chunks {
        let is_prose = matches!(
            chunk.content_type,
            ContentType::PlainParagraph
                | ContentType::ShortDisconnectedParagraph
                | ContentType::LongSingleParagraph
        );
        if is_prose && chunk.content.len() < min_chars {
            if let Some(prev) = result.last_mut() {
                let prev_prose = matches!(
                    prev.content_type,
                    ContentType::PlainParagraph
                        | ContentType::ShortDisconnectedParagraph
                        | ContentType::LongSingleParagraph
                );
                if prev_prose && prev.content.len() + chunk.content.len() < soft_max {
                    prev.content = format!("{}\n{}", prev.content, chunk.content)
                        .trim()
                        .to_string();
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

pub fn build_chunks_from_txt_bytes(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    // Detect the encoding instead of assuming UTF-8: a Notepad "Unicode" save is
    // UTF-16, "ANSI" is cp1252, and both used to come out as replacement
    // characters. Also strips a BOM, which otherwise leaks into the first chunk.
    let (text, _encoding) = crate::text_encoding::decode_text(bytes);

    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_txt_blocks(&text);
    let mut chunks: Vec<ChunkRecordInput> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut prose_parts: Vec<String> = Vec::new();
    let mut prose_len: usize = 0;

    for block in &blocks {
        match block.content_type {
            ContentType::HeadingSection => {
                flush_prose(
                    &mut chunks,
                    &current_heading,
                    &mut prose_parts,
                    &mut prose_len,
                );
                let heading_text = extract_heading_text(&block.content);
                current_heading = Some(heading_text.clone());
                chunks.push(ChunkRecordInput {
                    content_type: ContentType::HeadingSection,
                    content: heading_text,
                    metadata: txt_metadata(None),
                });
            }
            ContentType::Table | ContentType::CodeBlock | ContentType::BulletNumberedList => {
                flush_prose(
                    &mut chunks,
                    &current_heading,
                    &mut prose_parts,
                    &mut prose_len,
                );
                chunks.push(ChunkRecordInput {
                    content_type: block.content_type,
                    content: block.content.clone(),
                    metadata: txt_metadata(current_heading.clone()),
                });
            }
            _ => {
                let sub_blocks = if block.content.len() > MAX_CHUNK_CHARS {
                    split_at_sentences(&block.content, MAX_CHUNK_CHARS)
                } else {
                    vec![block.content.clone()]
                };
                for sub in sub_blocks {
                    let add = sub.len() + 1;
                    if prose_len + add > MAX_CHUNK_CHARS && !prose_parts.is_empty() {
                        flush_prose(
                            &mut chunks,
                            &current_heading,
                            &mut prose_parts,
                            &mut prose_len,
                        );
                    }
                    prose_len += add;
                    prose_parts.push(sub);
                }
            }
        }
    }

    flush_prose(
        &mut chunks,
        &current_heading,
        &mut prose_parts,
        &mut prose_len,
    );
    let chunks = merge_short_prose(chunks, MIN_CHUNK_CHARS);

    // A document that parsed cleanly but holds no extractable text is EMPTY,
    // not broken: return an empty list rather than an error. Raising here
    // conflated "nothing to chunk" with "could not read this file", and it was
    // inconsistent — docx/ppt/xlsx already returned `[]` for the same condition
    // while txt/html/md raised (TECH_DEBT T6). Structural invalidity still
    // errors: a PPTX with no slides, or a PDF with no text layer, both raise a
    // typed error carrying a remedy.
    Ok(chunks)
}
