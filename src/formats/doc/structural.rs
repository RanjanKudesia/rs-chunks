use std::path::Path;


use super::cfb_reader;
use super::fib;
use super::paragraph_props;
use super::piece_table;
use super::stylesheet;
use super::text_extractor::{self, DocParagraph, ParagraphType};
use crate::shared::{floor_char_boundary, split_at_sentences, split_sentences, STOPWORDS};

const MAX_CHUNK_CHARS: usize = 1200;
const MAX_SECTION_CHARS: usize = 2000;

#[derive(Debug, Clone)]
pub(crate) struct ChunkRecord {
    pub(crate) content: String,
    pub(crate) content_type: &'static str,
    pub(crate) chunk_index: usize,
    pub(crate) heading_level: Option<u8>,
    pub(crate) paragraph_type: &'static str,
}

fn paragraph_type_str(t: &ParagraphType) -> &'static str {
    match t {
        ParagraphType::Heading(_) => "heading",
        ParagraphType::Normal => "normal",
        ParagraphType::Table => "table",
        ParagraphType::ListItem => "list_item",
        ParagraphType::PageBreak => "page_break",
    }
}

fn content_type_for_paragraph(t: &ParagraphType, short: bool) -> &'static str {
    if short {
        return "short_disconnected_paragraph";
    }
    match t {
        ParagraphType::Heading(_) => "heading",
        ParagraphType::Normal => "plain_paragraph",
        ParagraphType::Table => "table",
        ParagraphType::ListItem => "bullet_list",
        ParagraphType::PageBreak => "plain_paragraph",
    }
}

pub(crate) fn validate_doc_path(file_path: &str) -> Result<(), String> {
    if !file_path.to_ascii_lowercase().ends_with(".doc") {
        return Err(format!("Expected .doc file path, got: {file_path}"));
    }
    if !Path::new(file_path).exists() {
        return Err(format!("File not found: {file_path}"));
    }
    Ok(())
}

pub(crate) fn load_doc_paragraphs(file_path: &str) -> Result<Vec<DocParagraph>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    load_doc_paragraphs_bytes(&bytes)
}

pub(crate) fn load_doc_paragraphs_bytes(bytes: &[u8]) -> Result<Vec<DocParagraph>, String> {
    let word_doc = cfb_reader::read_word_document_stream(bytes)?;
    let fib = fib::parse_fib(&word_doc)?;
    let table = cfb_reader::read_table_stream(bytes, fib.f_which_tbl_stm)?;
    let reconstructed = piece_table::parse_piece_table(
        &word_doc,
        &table,
        fib.fc_clx,
        fib.lcb_clx,
        fib.ccp_text,
    )?;
    let stylesheet = stylesheet::parse_stylesheet(&table, fib.fc_stshf, fib.lcb_stshf)?;
    let props = paragraph_props::parse_paragraph_props(
        &word_doc,
        &table,
        fib.fc_plcf_papx,
        fib.lcb_plcf_papx,
    )?;
    Ok(text_extractor::extract_paragraphs(
        &reconstructed,
        &props,
        &stylesheet,
    ))
}

/// Like [`load_doc_paragraphs`] but keeps each paragraph's raw ordinal, so
/// callers can interleave content anchored by raw paragraph index (used by
/// the image-aware markdown converter).
pub(crate) fn load_doc_paragraphs_indexed(
    file_path: &str,
) -> Result<Vec<(usize, DocParagraph)>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    load_doc_paragraphs_indexed_bytes(&bytes)
}

pub(crate) fn load_doc_paragraphs_indexed_bytes(
    bytes: &[u8],
) -> Result<Vec<(usize, DocParagraph)>, String> {
    let word_doc = cfb_reader::read_word_document_stream(bytes)?;
    let fib = fib::parse_fib(&word_doc)?;
    let table = cfb_reader::read_table_stream(bytes, fib.f_which_tbl_stm)?;
    let reconstructed = piece_table::parse_piece_table(
        &word_doc,
        &table,
        fib.fc_clx,
        fib.lcb_clx,
        fib.ccp_text,
    )?;
    let stylesheet = stylesheet::parse_stylesheet(&table, fib.fc_stshf, fib.lcb_stshf)?;
    let props = paragraph_props::parse_paragraph_props(
        &word_doc,
        &table,
        fib.fc_plcf_papx,
        fib.lcb_plcf_papx,
    )?;
    Ok(text_extractor::extract_paragraphs_indexed(
        &reconstructed,
        &props,
        &stylesheet,
    ))
}

/// Split an oversized paragraph so no chunk exceeds `max_chars`. Prefers
/// sentence boundaries; any residual piece still over the limit (text with no
/// detectable sentence boundaries) is hard-split at UTF-8 char boundaries so the
/// cap is always honoured.
fn split_oversized(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for piece in split_at_sentences(text, max_chars) {
        if piece.len() <= max_chars {
            if !piece.is_empty() {
                out.push(piece);
            }
            continue;
        }
        let bytes = piece.as_bytes();
        let mut start = 0usize;
        while start < bytes.len() {
            let mut end = (start + max_chars).min(piece.len());
            end = floor_char_boundary(&piece, end);
            if end <= start {
                // max_chars smaller than a single char; advance to next boundary.
                end = floor_char_boundary(&piece, start + 4).max(start + 1).min(piece.len());
            }
            let slice = piece[start..end].trim();
            if !slice.is_empty() {
                out.push(slice.to_string());
            }
            start = end;
        }
    }
    if out.is_empty() {
        vec![text.trim().to_string()]
    } else {
        out
    }
}

pub(crate) fn build_structural_chunks(paragraphs: Vec<DocParagraph>) -> Vec<ChunkRecord> {
    let mut out = Vec::new();
    let mut short_buf = String::new();

    for p in paragraphs {
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            if !short_buf.is_empty() {
                let content = short_buf.trim().to_string();
                if !content.is_empty() {
                    out.push(ChunkRecord {
                        content,
                        content_type: "short_disconnected_paragraph",
                        chunk_index: 0,
                        heading_level: None,
                        paragraph_type: "normal",
                    });
                }
                short_buf.clear();
            }
            continue;
        }

        let trimmed = p.content.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_short_normal = matches!(p.paragraph_type, ParagraphType::Normal) && trimmed.len() < 80;
        if is_short_normal {
            let candidate = if short_buf.is_empty() {
                trimmed.to_string()
            } else {
                format!("{}\n{}", short_buf, trimmed)
            };
            if candidate.len() > MAX_CHUNK_CHARS && !short_buf.is_empty() {
                out.push(ChunkRecord {
                    content: short_buf.trim().to_string(),
                    content_type: "short_disconnected_paragraph",
                    chunk_index: 0,
                    heading_level: None,
                    paragraph_type: "normal",
                });
                short_buf = trimmed.to_string();
            } else {
                short_buf = candidate;
            }
            continue;
        }

        if !short_buf.is_empty() {
            out.push(ChunkRecord {
                content: short_buf.trim().to_string(),
                content_type: "short_disconnected_paragraph",
                chunk_index: 0,
                heading_level: None,
                paragraph_type: "normal",
            });
            short_buf.clear();
        }

        let content_type = content_type_for_paragraph(&p.paragraph_type, false);
        let paragraph_type = paragraph_type_str(&p.paragraph_type);
        // A single paragraph can exceed MAX_CHUNK_CHARS (e.g. a document with no
        // paragraph breaks). Split it so we never emit an unsplittable mega-chunk,
        // mirroring how the docx chunker recursively splits oversized paragraphs.
        for piece in split_oversized(trimmed, MAX_CHUNK_CHARS) {
            out.push(ChunkRecord {
                content: piece,
                content_type,
                chunk_index: 0,
                heading_level: p.heading_level,
                paragraph_type,
            });
        }
    }

    if !short_buf.is_empty() {
        out.push(ChunkRecord {
            content: short_buf.trim().to_string(),
            content_type: "short_disconnected_paragraph",
            chunk_index: 0,
            heading_level: None,
            paragraph_type: "normal",
        });
    }

    for (i, ch) in out.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
    out
}

pub(crate) fn build_section_chunks(paragraphs: Vec<DocParagraph>) -> Vec<ChunkRecord> {
    let mut out = Vec::new();
    let mut current_heading = "Preamble".to_string();
    let mut current_level: Option<u8> = None;
    let mut lines: Vec<String> = Vec::new();

    let flush = |out: &mut Vec<ChunkRecord>, heading: &str, level: Option<u8>, lines: &mut Vec<String>| {
        if lines.is_empty() {
            return;
        }
        let joined = lines.join("\n").trim().to_string();
        lines.clear();
        if joined.is_empty() {
            return;
        }

        if joined.len() <= MAX_SECTION_CHARS {
            out.push(ChunkRecord {
                content: joined,
                content_type: "section",
                chunk_index: 0,
                heading_level: level,
                paragraph_type: if heading == "Preamble" { "normal" } else { "heading" },
            });
            return;
        }

        let mut start = 0usize;
        while start < joined.len() {
            // Snap to a UTF-8 char boundary so multi-byte content (e.g. '÷', CJK)
            // never triggers a mid-character slice panic on very large sections.
            let raw_end = (start + MAX_SECTION_CHARS).min(joined.len());
            let mut end = floor_char_boundary(&joined, raw_end);
            if end <= start {
                end = joined.len();
            }
            let part = joined[start..end].trim().to_string();
            if !part.is_empty() {
                out.push(ChunkRecord {
                    content: part,
                    content_type: "section",
                    chunk_index: 0,
                    heading_level: level,
                    paragraph_type: if heading == "Preamble" { "normal" } else { "heading" },
                });
            }
            start = end;
        }
    };

    for p in paragraphs {
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            flush(&mut out, &current_heading, current_level, &mut lines);
            continue;
        }

        let content = p.content.trim();
        if content.is_empty() {
            continue;
        }

        if let ParagraphType::Heading(level) = p.paragraph_type {
            flush(&mut out, &current_heading, current_level, &mut lines);
            current_heading = content.to_string();
            current_level = Some(level);
            lines.push(content.to_string());
        } else {
            lines.push(content.to_string());
        }
    }

    flush(&mut out, &current_heading, current_level, &mut lines);

    for (i, ch) in out.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
    out
}

fn keyword_set(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| w.len() >= 4)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

pub(crate) fn build_semantic_chunks(paragraphs: Vec<DocParagraph>) -> Vec<ChunkRecord> {
    let filtered: Vec<DocParagraph> = paragraphs
        .into_iter()
        .filter(|p| !matches!(p.paragraph_type, ParagraphType::PageBreak))
        .filter(|p| !p.content.trim().is_empty())
        .collect();

    if filtered.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_keywords = std::collections::HashSet::new();
    let mut current_heading_level = None;
    let mut current_para_type = "normal";

    let flush = |out: &mut Vec<ChunkRecord>, content: &mut String, heading_level: Option<u8>, para_type: &'static str| {
        let val = content.trim().to_string();
        if val.is_empty() {
            return;
        }
        out.push(ChunkRecord {
            content: val,
            content_type: "semantic",
            chunk_index: 0,
            heading_level,
            paragraph_type: para_type,
        });
        content.clear();
    };

    for p in filtered {
        let text = p.content.trim();
        let keys = keyword_set(text);

        let is_heading = matches!(p.paragraph_type, ParagraphType::Heading(_));
        let overlaps = !current_keywords.is_empty() && current_keywords.intersection(&keys).next().is_some();
        let starts_reference = {
            let lower = text.to_ascii_lowercase();
            ["this ", "it ", "they ", "these ", "that ", "those "]
                .iter()
                .any(|x| lower.starts_with(x))
        };

        let should_split = is_heading
            || (!current.is_empty()
                && !overlaps
                && !starts_reference
                && current.len() + 2 + text.len() > MAX_CHUNK_CHARS);

        if should_split {
            flush(&mut out, &mut current, current_heading_level, current_para_type);
            current_keywords.clear();
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(text);
        current_keywords.extend(keys);
        current_heading_level = p.heading_level;
        current_para_type = paragraph_type_str(&p.paragraph_type);
    }

    flush(&mut out, &mut current, current_heading_level, current_para_type);

    for (i, ch) in out.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
    out
}

pub(crate) fn build_sliding_window_chunks(
    paragraphs: Vec<DocParagraph>,
    window_size: usize,
    overlap: usize,
) -> Vec<ChunkRecord> {
    if window_size == 0 || overlap >= window_size {
        return Vec::new();
    }

    let items: Vec<DocParagraph> = paragraphs
        .into_iter()
        .filter(|p| !matches!(p.paragraph_type, ParagraphType::PageBreak))
        .filter(|p| !p.content.trim().is_empty())
        .collect();
    if items.is_empty() {
        return Vec::new();
    }

    let step = window_size - overlap;
    let mut out = Vec::new();
    let mut start = 0usize;

    while start < items.len() {
        let end = (start + window_size).min(items.len());
        let window = &items[start..end];
        let content = window
            .iter()
            .map(|p| p.content.trim().to_string())
            .collect::<Vec<_>>()
            .join("\n\n");
        out.push(ChunkRecord {
            content,
            content_type: "sliding_window",
            chunk_index: 0,
            heading_level: window.iter().find_map(|p| p.heading_level),
            paragraph_type: paragraph_type_str(&window[0].paragraph_type),
        });

        if end == items.len() {
            break;
        }
        start += step;
    }

    for (i, ch) in out.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
    out
}

pub(crate) fn build_sentence_chunks(
    paragraphs: Vec<DocParagraph>,
    sentences_per_chunk: usize,
) -> Vec<ChunkRecord> {
    if sentences_per_chunk == 0 {
        return Vec::new();
    }

    let mut indexed_sentences: Vec<(String, Option<u8>, &'static str)> = Vec::new();
    for p in paragraphs {
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            continue;
        }
        let text = p.content.trim();
        if text.is_empty() {
            continue;
        }
        let mut sentences = split_sentences(text);
        if sentences.is_empty() {
            sentences.push(text.to_string());
        }
        for s in sentences {
            indexed_sentences.push((s, p.heading_level, paragraph_type_str(&p.paragraph_type)));
        }
    }

    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < indexed_sentences.len() {
        let end = (idx + sentences_per_chunk).min(indexed_sentences.len());
        let window = &indexed_sentences[idx..end];
        let content = window
            .iter()
            .map(|(s, _, _)| s.clone())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(ChunkRecord {
            content,
            content_type: "sentence",
            chunk_index: 0,
            heading_level: window[0].1,
            paragraph_type: window[0].2,
        });
        idx = end;
    }

    for (i, ch) in out.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
    out
}

pub(crate) fn build_page_aware_chunks(
    paragraphs: Vec<DocParagraph>,
    paragraphs_per_page: usize,
) -> Vec<ChunkRecord> {
    if paragraphs_per_page == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut acc: Vec<DocParagraph> = Vec::new();

    let flush = |out: &mut Vec<ChunkRecord>, acc: &mut Vec<DocParagraph>| {
        if acc.is_empty() {
            return;
        }
        let content = acc
            .iter()
            .map(|p| p.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !content.is_empty() {
            out.push(ChunkRecord {
                content,
                content_type: "page_aware",
                chunk_index: 0,
                heading_level: acc.iter().find_map(|p| p.heading_level),
                paragraph_type: paragraph_type_str(&acc[0].paragraph_type),
            });
        }
        acc.clear();
    };

    for p in paragraphs {
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            flush(&mut out, &mut acc);
            continue;
        }
        if p.content.trim().is_empty() {
            continue;
        }
        acc.push(p);
        let normal_count = acc
            .iter()
            .filter(|x| matches!(x.paragraph_type, ParagraphType::Normal))
            .count();
        if normal_count >= paragraphs_per_page {
            flush(&mut out, &mut acc);
        }
    }
    flush(&mut out, &mut acc);

    for (i, ch) in out.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
    out
}

