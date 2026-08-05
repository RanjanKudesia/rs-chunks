/// Sentence chunker for Markdown.
///
/// Extracts prose text from paragraphs and lists, splits it into sentences
/// using punctuation rules (no NLP), then groups N sentences per chunk.
/// Code blocks and tables are emitted as standalone chunks — they are not
/// broken into sentences.  Headings are emitted standalone with content_type
/// "heading".
///
/// Sentence boundary detection handles common abbreviations:
///   Mr. Mrs. Dr. Prof. Sr. Jr. vs. etc.
///   Numeric markers (e.g. "Figure 1.")
///   Initials (e.g. "J.K.")
///
/// Metadata per chunk:
///   sentences_per_chunk    — target N
///   actual_sentence_count  — sentences actually included (last chunk may be fewer)
///   chunk_index            — 0-based position among all sentence chunks
///   source_paragraph_index — block index of the first sentence's source
///   section_heading        — heading context at emission time
///   heading_path           — full breadcrumb

use serde_json::json;

use super::common::{
    current_section_heading, current_section_level, extract_heading_text, heading_level,
    heading_path_strings, parse_markdown_blocks, strip_block_content, update_heading_stack,
    ChunkRecordInput, ContentType, MdBlockType, SpannedRecord,
};

// ── Sentence splitting ────────────────────────────────────────────────────────

fn ends_with_title_abbreviation(prefix: &str) -> bool {
    let lower = prefix.trim_end().to_ascii_lowercase();
    ["mr.", "mrs.", "dr.", "prof.", "sr.", "jr.", "vs.", "etc.", "approx.", "est."]
        .iter()
        .any(|abbr| lower.ends_with(abbr))
}

fn ends_with_numeric_marker(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let without_dot = trimmed.strip_suffix('.').unwrap_or(trimmed);
    let token = without_dot
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .trim();
    !token.is_empty() && token.chars().all(|c| c.is_ascii_digit())
}

fn ends_with_initials(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 4 || *bytes.last().unwrap_or(&b' ') != b'.' {
        return false;
    }
    let mut i = bytes.len();
    let mut groups = 0usize;
    while i >= 2 {
        if bytes[i - 1] != b'.' || !bytes[i - 2].is_ascii_alphabetic() {
            break;
        }
        groups += 1;
        if i < 3 {
            break;
        }
        i -= 2;
        if i == 0 || bytes[i - 1] != b'.' {
            break;
        }
        i -= 1;
    }
    groups >= 2
}

fn should_split_at(chars: &[char], punct_idx: usize, punct: char) -> bool {
    // Need at least one char after ". X"
    if punct_idx + 2 >= chars.len() {
        return false;
    }
    // Must be followed by space then uppercase.
    if chars[punct_idx + 1] != ' ' || !chars[punct_idx + 2].is_uppercase() {
        return false;
    }
    if punct == '.' {
        let prefix: String = chars[..=punct_idx].iter().collect();
        if ends_with_title_abbreviation(&prefix)
            || ends_with_numeric_marker(&prefix)
            || ends_with_initials(&prefix)
        {
            return false;
        }
    }
    true
}

fn split_into_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let len = chars.len();
    let mut i = 0usize;

    while i < len {
        let ch = chars[i];
        if matches!(ch, '.' | '?' | '!') && should_split_at(&chars, i, ch) {
            let sentence: String = chars[start..=i].iter().collect();
            let sentence = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
            if !sentence.is_empty() {
                out.push(sentence);
            }
            let mut next = i + 1;
            while next < len && chars[next].is_whitespace() {
                next += 1;
            }
            start = next;
            i = next;
            continue;
        }
        i += 1;
    }
    if start < len {
        let tail: String = chars[start..].iter().collect();
        let tail = tail.split_whitespace().collect::<Vec<_>>().join(" ");
        if !tail.is_empty() {
            out.push(tail);
        }
    }
    out
}

// ── Indexed sentence ──────────────────────────────────────────────────────────

struct IndexedSentence {
    text: String,
    /// The markdown block this sentence was split out of.
    block: usize,
    paragraph_index: usize,
    section_heading: Option<String>,
    heading_path: Vec<String>,
    section_level: u8,
}

// ── Core algorithm ────────────────────────────────────────────────────────────

pub fn build_sentence_chunks(
    bytes: &[u8],
    sentences_per_chunk: usize,
) -> Result<Vec<SpannedRecord>, String> {
    if sentences_per_chunk == 0 {
        return Err("sentences_per_chunk must be greater than 0".to_string());
    }

    let text = std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string());
    if text.trim().is_empty() {
        return Err("Markdown file is empty".to_string());
    }

    let blocks = parse_markdown_blocks(&text);
    let total_input_blocks = blocks.len();
    let mut result: Vec<SpannedRecord> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut sentences: Vec<IndexedSentence> = Vec::new();
    let mut para_index = 0usize;
    let mut chunk_index = 0usize;

    // Helper: flush accumulated sentence buffer as grouped sentence chunks.
    let flush_sentences =
        |sentences: &mut Vec<IndexedSentence>,
         result: &mut Vec<SpannedRecord>,
         chunk_index: &mut usize,
         spc: usize,
         total: usize| {
            let mut i = 0usize;
            while i < sentences.len() {
                let end = (i + spc).min(sentences.len());
                let window = &sentences[i..end];
                let content = window.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
                if !content.is_empty() {
                    let span = Some((window[0].block, window[window.len() - 1].block));
                    result.push(SpannedRecord::spanning(ChunkRecordInput {
                        content_type: ContentType::Sentence,
                        content,
                        metadata: json!({
                            "sentences_per_chunk":    spc,
                            "actual_sentence_count":  window.len(),
                            "chunk_index":            *chunk_index,
                            "source_paragraph_index": window[0].paragraph_index,
                            "section_heading":        window[0].section_heading,
                            "heading_path":           window[0].heading_path,
                            "section_level":          window[0].section_level,
                            "document_metadata": {
                                "source_type":        "md",
                                "total_input_blocks": total,
                            }
                        }),
                    }, span));
                    *chunk_index += 1;
                }
                i = end;
            }
            sentences.clear();
        };

    for block in blocks {
        match block.block_type {
            MdBlockType::Heading => {
                // Flush sentences accumulated so far.
                flush_sentences(
                    &mut sentences,
                    &mut result,
                    &mut chunk_index,
                    sentences_per_chunk,
                    total_input_blocks,
                );

                let level = heading_level(&block.content);
                let text = extract_heading_text(&block.content);
                update_heading_stack(&mut heading_stack, level, text.clone());

                result.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::HeadingSection,
                    content: text.clone(),
                    metadata: json!({
                        "section_heading":    text,
                        "section_level":      level,
                        "heading_path":       heading_path_strings(&heading_stack),
                        "chunk_index":        chunk_index,
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                }, block.index));
                chunk_index += 1;
            }

            MdBlockType::Code => {
                // Code blocks are standalone — not split into sentences.
                flush_sentences(
                    &mut sentences,
                    &mut result,
                    &mut chunk_index,
                    sentences_per_chunk,
                    total_input_blocks,
                );
                result.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::CodeBlock,
                    content: block.content.clone(),
                    metadata: json!({
                        "section_heading":    current_section_heading(&heading_stack),
                        "heading_path":       heading_path_strings(&heading_stack),
                        "section_level":      current_section_level(&heading_stack),
                        "chunk_index":        chunk_index,
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                }, block.index));
                chunk_index += 1;
                para_index += 1;
            }

            MdBlockType::Table => {
                flush_sentences(
                    &mut sentences,
                    &mut result,
                    &mut chunk_index,
                    sentences_per_chunk,
                    total_input_blocks,
                );
                result.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::Table,
                    content: block.content.clone(),
                    metadata: json!({
                        "section_heading":    current_section_heading(&heading_stack),
                        "heading_path":       heading_path_strings(&heading_stack),
                        "section_level":      current_section_level(&heading_stack),
                        "chunk_index":        chunk_index,
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                }, block.index));
                chunk_index += 1;
                para_index += 1;
            }

            MdBlockType::Paragraph => {
                let clean = strip_block_content(&block.content, false);
                if clean.is_empty() {
                    continue;
                }
                let sh = current_section_heading(&heading_stack);
                let hp = heading_path_strings(&heading_stack);
                let sl = current_section_level(&heading_stack);
                let current_para_index = para_index;
                for s in split_into_sentences(&clean) {
                    sentences.push(IndexedSentence {
                        text: s,
                        block: block.index,
                        paragraph_index: current_para_index,
                        section_heading: sh.clone(),
                        heading_path: hp.clone(),
                        section_level: sl,
                    });
                }
                para_index += 1;
            }

            MdBlockType::List => {
                let clean = strip_block_content(&block.content, true);
                if clean.is_empty() {
                    continue;
                }
                // Flush sentences accumulated so far, then emit the list as a
                // standalone chunk — list items rarely end with sentence-terminal
                // punctuation and produce poor sentence fragments when split.
                flush_sentences(
                    &mut sentences,
                    &mut result,
                    &mut chunk_index,
                    sentences_per_chunk,
                    total_input_blocks,
                );
                result.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::BulletNumberedList,
                    content: clean,
                    metadata: json!({
                        "sentences_per_chunk":    sentences_per_chunk,
                        "actual_sentence_count":  0,
                        "chunk_index":            chunk_index,
                        "source_paragraph_index": para_index,
                        "section_heading":        current_section_heading(&heading_stack),
                        "heading_path":           heading_path_strings(&heading_stack),
                        "section_level":          current_section_level(&heading_stack),
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                }, block.index));
                chunk_index += 1;
                para_index += 1;
            }
        }
    }

    // Flush remaining sentences.
    flush_sentences(
        &mut sentences,
        &mut result,
        &mut chunk_index,
        sentences_per_chunk,
        total_input_blocks,
    );

    if result.is_empty() {
        return Err("No sentence chunks generated".to_string());
    }
    Ok(result)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────

