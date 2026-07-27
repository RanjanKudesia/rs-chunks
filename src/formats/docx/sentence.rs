use serde_json::{json, Value};

use super::common::{
    collapse_whitespace,
    parse_docx_indexed_paragraphs, IndexedParagraph,
};

const TITLE_ABBREVIATIONS: [&str; 22] = [
    "mr.", "mrs.", "ms.", "dr.", "prof.", "sr.", "jr.", "vs.", "etc.", "st.", "capt.", "lt.",
    "sgt.", "gen.", "col.", "rev.", "hon.", "gov.", "sen.", "rep.", "fig.", "vol.",
];

/// Characters allowed to trail a terminal `.` / `?` / `!` and still count as
/// part of the same sentence (closing quotes, parentheses, brackets).
const SENTENCE_CLOSERS: &[char] = &['"', '\u{201D}', '\'', '\u{2019}', ')', ']'];

#[derive(Debug, Clone)]
struct IndexedSentence {
    paragraph_index: usize,
    text: String,
    paragraph_is_heading: bool,
    paragraph_heading_level: Option<u32>,
    paragraph_is_list: bool,
    paragraph_is_table: bool,
}

#[derive(Debug, Clone)]
struct ChunkRecordInput {
    content: String,
    metadata: Value,
}

fn build_sentence_chunks(
    sentences: Vec<IndexedSentence>,
    sentences_per_chunk: usize,
) -> Vec<ChunkRecordInput> {
    if sentences.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut chunk_index = 0usize;

    while start < sentences.len() {
        let end = (start + sentences_per_chunk).min(sentences.len());
        let window = &sentences[start..end];
        let content = window
            .iter()
            .map(|sentence| sentence.text.clone())
            .collect::<Vec<_>>()
            .join(" ");

        chunks.push(ChunkRecordInput {
            content,
            metadata: json!({
                "sentences_per_chunk": sentences_per_chunk,
                "actual_sentence_count": window.len(),
                "chunk_index": chunk_index,
                "source_paragraph_index": window[0].paragraph_index,
                "source_paragraph_is_heading": window[0].paragraph_is_heading,
                "source_paragraph_heading_level": window[0].paragraph_heading_level,
                "source_paragraph_is_list": window[0].paragraph_is_list,
                "source_paragraph_is_table": window[0].paragraph_is_table,
                "document_metadata": {
                    "source_type": "docx"
                }
            }),
        });

        start = end;
        chunk_index += 1;
    }

    chunks
}

fn split_paragraph_sentences(paragraph: &IndexedParagraph) -> Vec<IndexedSentence> {
    let chars: Vec<char> = paragraph.text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    let len = chars.len();
    let mut i = 0usize;

    while i < len {
        let ch = chars[i];
        if matches!(ch, '.' | '?' | '!') {
            if let Some(end_inclusive) = sentence_end_at(&chars, i, ch) {
                let sentence = chars[start..=end_inclusive].iter().collect::<String>();
                let sentence = collapse_whitespace(&sentence);
                if !sentence.is_empty() {
                    out.push(IndexedSentence {
                        paragraph_index: paragraph.index,
                        text: sentence,
                        paragraph_is_heading: paragraph.is_heading,
                        paragraph_heading_level: paragraph.heading_level,
                        paragraph_is_list: paragraph.is_list,
                        paragraph_is_table: paragraph.is_table,
                    });
                }

                let mut next_start = end_inclusive + 1;
                while next_start < len && chars[next_start].is_whitespace() {
                    next_start += 1;
                }
                start = next_start;
                i = next_start;
                continue;
            }
        }
        i += 1;
    }

    if start < len {
        let tail = chars[start..].iter().collect::<String>();
        let tail = collapse_whitespace(&tail);
        if !tail.is_empty() {
            out.push(IndexedSentence {
                paragraph_index: paragraph.index,
                text: tail,
                paragraph_is_heading: paragraph.is_heading,
                paragraph_heading_level: paragraph.heading_level,
                paragraph_is_list: paragraph.is_list,
                paragraph_is_table: paragraph.is_table,
            });
        }
    }

    out
}

/// Determine whether a `.` / `?` / `!` at `punct_idx` ends a sentence, and if
/// so, return the inclusive end index (which may be past `punct_idx` when
/// trailing closing quotes/parens belong to the current sentence).
fn sentence_end_at(chars: &[char], punct_idx: usize, punct: char) -> Option<usize> {
    // Walk over any trailing closing quotes/parens that should stay with the
    // current sentence (e.g. `He said "Hello." Then left.`).
    let mut end = punct_idx;
    let mut after = punct_idx + 1;
    while after < chars.len() && SENTENCE_CLOSERS.contains(&chars[after]) {
        end = after;
        after += 1;
    }

    // Need at least one whitespace + uppercase letter after the (possibly
    // extended) sentence end to count as a boundary.
    if after + 1 >= chars.len() {
        return None;
    }
    if chars[after] != ' ' || !chars[after + 1].is_uppercase() {
        return None;
    }

    if punct == '.' {
        let prefix = chars[..=punct_idx].iter().collect::<String>();
        if ends_with_title_abbreviation(&prefix)
            || ends_with_list_marker(&prefix)
            || ends_with_initials_abbreviation(&prefix)
        {
            return None;
        }
    }

    Some(end)
}

fn ends_with_title_abbreviation(prefix: &str) -> bool {
    let lower = prefix.trim_end().to_ascii_lowercase();
    TITLE_ABBREVIATIONS.iter().any(|abbr| lower.ends_with(abbr))
}

/// True when `prefix` ends with a short numeric list/ordinal marker such as
/// `1.`, `12.`, or `999.`. Restricted to 1–3 digit tokens so that years
/// (`1985.`) and longer numbers still terminate sentences correctly.
fn ends_with_list_marker(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let without_dot = match trimmed.strip_suffix('.') {
        Some(s) => s,
        None => return false,
    };
    let token = without_dot
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .trim();
    if token.is_empty() || token.len() > 3 {
        return false;
    }
    token.chars().all(|c| c.is_ascii_digit())
}

fn ends_with_initials_abbreviation(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 4 || *bytes.last().unwrap_or(&b' ') != b'.' {
        return false;
    }

    // Match patterns like U.S. / U.K. / U.S.A.
    let mut i = bytes.len();
    let mut groups = 0usize;

    while i >= 2 {
        if bytes[i - 1] != b'.' {
            break;
        }
        if !bytes[i - 2].is_ascii_uppercase() {
            break;
        }
        groups += 1;
        if i < 3 {
            break;
        }
        i -= 2;
        if i > 0 && bytes[i - 1] == b'.' {
            continue;
        }
    }

    groups >= 2
}


pub(super) fn chunk(bytes: &[u8], sentences_per_chunk: usize) -> Result<Vec<crate::chunk::Chunk>, String> {
    let paragraphs = parse_docx_indexed_paragraphs(bytes)?;
    let sentences = paragraphs
        .iter()
        .flat_map(|paragraph| split_paragraph_sentences(paragraph))
        .collect::<Vec<_>>();
    Ok(build_sentence_chunks(sentences, sentences_per_chunk)
        .into_iter()
        .map(|c| crate::chunk::Chunk::new(c.content, "sentence", c.metadata))
        .collect())
}

pub(super) fn chunk_with_images(bytes: &[u8], sentences_per_chunk: usize) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    let (mut archive, image_rids_map) = super::common::open_docx_archive_with_rids(bytes)?;
    let items = super::common::parse_docx_indexed_items_with_images(bytes)?;
    let mut text_paragraphs: Vec<IndexedParagraph> = Vec::new();
    let mut image_items: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut para_index = 0usize;
    for item in items {
        match item {
            super::common::ParaOrImage::Para(ev) => {
                text_paragraphs.push(IndexedParagraph {
                    index: para_index, text: ev.text, is_heading: ev.is_heading,
                    heading_level: ev.heading_level, is_list: ev.is_list, is_table: ev.is_table,
                });
                para_index += 1;
            }
            super::common::ParaOrImage::Image { rid, alt, .. } => image_items.push((rid, alt)),
        }
    }
    let sentences = text_paragraphs.iter().flat_map(split_paragraph_sentences).collect::<Vec<_>>();
    let text_chunks = build_sentence_chunks(sentences, sentences_per_chunk);
    let (entries, image_out) = super::common::collect_image_chunks_from_items(image_items, &image_rids_map, &mut archive);
    let mut chunks: Vec<crate::chunk::Chunk> = entries.into_iter().map(|(n, m)| crate::chunk::Chunk::new(n, "image", m)).collect();
    for c in text_chunks { chunks.push(crate::chunk::Chunk::new(c.content, "sentence", c.metadata)); }
    Ok((chunks, image_out))
}
