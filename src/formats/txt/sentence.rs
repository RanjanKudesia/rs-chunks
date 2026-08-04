use serde_json::json;

use super::common::{
    current_section_heading, extract_heading_text, heading_level_txt, heading_path_strings,
    parse_txt_blocks, split_sentences, update_heading_stack, ChunkRecordInput, ContentType,
};

struct IndexedSentence {
    text: String,
    paragraph_index: usize,
    section_heading: Option<String>,
    heading_path: Vec<String>,
}

pub fn build_sentence_chunks(
    bytes: &[u8],
    sentences_per_chunk: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    if sentences_per_chunk == 0 { return Err("sentences_per_chunk must be greater than 0".to_string()); }

    let text = crate::text_encoding::decode_text(bytes).0;
    if text.trim().is_empty() { return Err("TXT file is empty".to_string()); }

    let blocks = parse_txt_blocks(&text);
    let total = blocks.len();
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut sentences: Vec<IndexedSentence> = Vec::new();
    let mut para_index = 0usize;
    let mut chunk_index = 0usize;

    let flush_sentences = |sentences: &mut Vec<IndexedSentence>,
                           result: &mut Vec<ChunkRecordInput>,
                           ci: &mut usize,
                           spc: usize,
                           total: usize| {
        let mut i = 0usize;
        while i < sentences.len() {
            let end = (i + spc).min(sentences.len());
            let window = &sentences[i..end];
            let content = window.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
            if !content.is_empty() {
                result.push(ChunkRecordInput {
                    content_type: ContentType::Sentence,
                    content,
                    metadata: json!({
                        "sentences_per_chunk":    spc,
                        "actual_sentence_count":  window.len(),
                        "chunk_index":            *ci,
                        "source_paragraph_index": window[0].paragraph_index,
                        "section_heading":        window[0].section_heading,
                        "heading_path":           window[0].heading_path,
                        "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                    }),
                });
                *ci += 1;
            }
            i = end;
        }
        sentences.clear();
    };

    let is_prose = |ct: ContentType| matches!(
        ct,
        ContentType::PlainParagraph | ContentType::LongSingleParagraph | ContentType::ShortDisconnectedParagraph
    );

    for block in &blocks {
        if block.content_type == ContentType::HeadingSection {
            flush_sentences(&mut sentences, &mut result, &mut chunk_index, sentences_per_chunk, total);
            let level = heading_level_txt(&block.content);
            let text = extract_heading_text(&block.content);
            update_heading_stack(&mut heading_stack, level, text.clone());
            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection,
                content: text.clone(),
                metadata: json!({
                    "section_heading": text,
                    "section_level":   level,
                    "heading_path":    heading_path_strings(&heading_stack),
                    "chunk_index":     chunk_index,
                    "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                }),
            });
            chunk_index += 1;
        } else if block.content_type == ContentType::CodeBlock || block.content_type == ContentType::Table {
            flush_sentences(&mut sentences, &mut result, &mut chunk_index, sentences_per_chunk, total);
            result.push(ChunkRecordInput {
                content_type: block.content_type,
                content: block.content.clone(),
                metadata: json!({
                    "section_heading": current_section_heading(&heading_stack),
                    "heading_path":    heading_path_strings(&heading_stack),
                    "chunk_index":     chunk_index,
                    "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                }),
            });
            chunk_index += 1;
            para_index += 1;
        } else if block.content_type == ContentType::BulletNumberedList {
            // List items become individual "sentences".
            let sh = current_section_heading(&heading_stack);
            let hp = heading_path_strings(&heading_stack);
            let pi = para_index;
            for line in block.content.lines() {
                let t = line.trim().to_string();
                if !t.is_empty() {
                    sentences.push(IndexedSentence {
                        text: t,
                        paragraph_index: pi,
                        section_heading: sh.clone(),
                        heading_path: hp.clone(),
                    });
                }
            }
            para_index += 1;
        } else if is_prose(block.content_type) {
            let sh = current_section_heading(&heading_stack);
            let hp = heading_path_strings(&heading_stack);
            let pi = para_index;
            for s in split_sentences(&block.content) {
                sentences.push(IndexedSentence {
                    text: s, paragraph_index: pi,
                    section_heading: sh.clone(), heading_path: hp.clone(),
                });
            }
            para_index += 1;
        }
    }
    flush_sentences(&mut sentences, &mut result, &mut chunk_index, sentences_per_chunk, total);
    if result.is_empty() { return Err("No sentence chunks generated".to_string()); }
    Ok(result)
}

