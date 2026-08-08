//! The six chunk builders shared by `.doc` and `.ppt`.
//!
//! Every builder consumes a positioned paragraph list (see [`super::context`])
//! and emits `ChunkRecord`s. A chunk takes the structural position of the
//! paragraph it *starts* on; the aggregating builders record the position where
//! their accumulation began rather than where it flushed.

use super::context::{context_of, position, ChunkContext, Positioned};
use super::text_extractor::{DocParagraph, ParagraphType};
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
    /// Where in the document this chunk starts — page/slide, section
    /// breadcrumb, list depth and table shape (TECH_DEBT #11, #18, #12).
    pub(crate) context: ChunkContext,
}

impl ChunkRecord {
    fn new(
        content: String,
        content_type: &'static str,
        paragraph_type: &'static str,
        heading_level: Option<u8>,
        context: ChunkContext,
    ) -> Self {
        ChunkRecord {
            content,
            content_type,
            chunk_index: 0,
            heading_level,
            paragraph_type,
            context,
        }
    }

    /// The shape every aggregating builder flushes: text gathered from several
    /// paragraphs, carrying the context where the gathering began.
    fn aggregate(
        content: String,
        content_type: &'static str,
        paragraph_type: &'static str,
        heading_level: Option<u8>,
        context: ChunkContext,
    ) -> Self {
        ChunkRecord::new(content, content_type, paragraph_type, heading_level, context)
    }
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

fn number(records: &mut [ChunkRecord]) {
    for (i, ch) in records.iter_mut().enumerate() {
        ch.chunk_index = i;
    }
}

/// Drop page breaks and empty paragraphs — what every non-page-aware builder
/// wants to iterate.
fn content_paragraphs(paragraphs: Vec<DocParagraph>) -> Vec<Positioned> {
    position(paragraphs)
        .into_iter()
        .filter(|p| !matches!(p.paragraph.paragraph_type, ParagraphType::PageBreak))
        .filter(|p| !p.paragraph.content.trim().is_empty())
        .collect()
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
    // Short paragraphs aggregate across page breaks, so the resulting chunk
    // belongs to where the *buffer* started, not where it flushed.
    let mut short_ctx = ChunkContext::default();

    let flush_short = |out: &mut Vec<ChunkRecord>, buf: &mut String, ctx: &ChunkContext| {
        let content = buf.trim().to_string();
        buf.clear();
        if content.is_empty() {
            return;
        }
        out.push(ChunkRecord::aggregate(
            content,
            "short_disconnected_paragraph",
            "normal",
            None,
            ctx.clone(),
        ));
    };

    for item in position(paragraphs) {
        let p = &item.paragraph;
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            if !short_buf.is_empty() {
                flush_short(&mut out, &mut short_buf, &short_ctx);
            }
            continue;
        }

        let trimmed = p.content.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_short_normal = matches!(p.paragraph_type, ParagraphType::Normal) && trimmed.len() < 80;
        if is_short_normal {
            let buf_was_empty = short_buf.is_empty();
            let candidate = if buf_was_empty {
                trimmed.to_string()
            } else {
                format!("{}\n{}", short_buf, trimmed)
            };
            if candidate.len() > MAX_CHUNK_CHARS && !short_buf.is_empty() {
                flush_short(&mut out, &mut short_buf, &short_ctx);
                short_buf = trimmed.to_string();
                short_ctx = item.context.clone();
            } else {
                if buf_was_empty {
                    short_ctx = item.context.clone();
                }
                short_buf = candidate;
            }
            continue;
        }

        if !short_buf.is_empty() {
            flush_short(&mut out, &mut short_buf, &short_ctx);
        }

        let content_type = content_type_for_paragraph(&p.paragraph_type, false);
        let paragraph_type = paragraph_type_str(&p.paragraph_type);
        // A single paragraph can exceed MAX_CHUNK_CHARS (e.g. a document with no
        // paragraph breaks). Split it so we never emit an unsplittable mega-chunk,
        // mirroring how the docx chunker recursively splits oversized paragraphs.
        for piece in split_oversized(trimmed, MAX_CHUNK_CHARS) {
            out.push(ChunkRecord::new(
                piece,
                content_type,
                paragraph_type,
                p.heading_level,
                item.context.clone(),
            ));
        }
    }

    if !short_buf.is_empty() {
        flush_short(&mut out, &mut short_buf, &short_ctx);
    }

    number(&mut out);
    out
}

pub(crate) fn build_section_chunks(paragraphs: Vec<DocParagraph>) -> Vec<ChunkRecord> {
    let mut out = Vec::new();
    let mut current_heading = "Preamble".to_string();
    let mut current_level: Option<u8> = None;
    let mut lines: Vec<String> = Vec::new();

    // The position a section *starts* at. `lines` is a flat Vec<String> with no
    // provenance of its own, so the caller records it when the section opens.
    let mut section_ctx = ChunkContext::default();

    let flush = |out: &mut Vec<ChunkRecord>,
                 heading: &str,
                 level: Option<u8>,
                 lines: &mut Vec<String>,
                 ctx: &ChunkContext| {
        if lines.is_empty() {
            return;
        }
        let joined = lines.join("\n").trim().to_string();
        lines.clear();
        if joined.is_empty() {
            return;
        }
        let paragraph_type = if heading == "Preamble" { "normal" } else { "heading" };

        if joined.len() <= MAX_SECTION_CHARS {
            out.push(ChunkRecord::aggregate(
                joined,
                "section",
                paragraph_type,
                level,
                ctx.clone(),
            ));
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
                out.push(ChunkRecord::aggregate(
                    part,
                    "section",
                    paragraph_type,
                    level,
                    ctx.clone(),
                ));
            }
            start = end;
        }
    };

    for item in position(paragraphs) {
        let p = &item.paragraph;
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            flush(&mut out, &current_heading, current_level, &mut lines, &section_ctx);
            section_ctx = ChunkContext::default();
            continue;
        }

        let content = p.content.trim();
        if content.is_empty() {
            continue;
        }

        if let ParagraphType::Heading(level) = p.paragraph_type {
            flush(&mut out, &current_heading, current_level, &mut lines, &section_ctx);
            current_heading = content.to_string();
            current_level = Some(level);
            section_ctx = item.context.clone();
            lines.push(content.to_string());
        } else {
            if lines.is_empty() {
                section_ctx = item.context.clone();
            }
            lines.push(content.to_string());
        }
    }

    flush(&mut out, &current_heading, current_level, &mut lines, &section_ctx);

    number(&mut out);
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
    let filtered = content_paragraphs(paragraphs);
    if filtered.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_keywords = std::collections::HashSet::new();
    let mut current_heading_level = None;
    let mut current_para_type = "normal";
    // Position the accumulating chunk started at; `current` is a flat String.
    let mut current_ctx = ChunkContext::default();

    let flush = |out: &mut Vec<ChunkRecord>,
                 content: &mut String,
                 heading_level: Option<u8>,
                 para_type: &'static str,
                 ctx: &ChunkContext| {
        let val = content.trim().to_string();
        if val.is_empty() {
            return;
        }
        out.push(ChunkRecord::aggregate(
            val,
            "semantic",
            para_type,
            heading_level,
            ctx.clone(),
        ));
        content.clear();
    };

    for item in filtered {
        let p = &item.paragraph;
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
            flush(&mut out, &mut current, current_heading_level, current_para_type, &current_ctx);
            current_keywords.clear();
        }

        if current.is_empty() {
            current_ctx = item.context.clone();
        } else {
            current.push_str("\n\n");
        }
        current.push_str(text);
        current_keywords.extend(keys);
        current_heading_level = p.heading_level;
        current_para_type = paragraph_type_str(&p.paragraph_type);
    }

    flush(&mut out, &mut current, current_heading_level, current_para_type, &current_ctx);

    number(&mut out);
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

    let items = content_paragraphs(paragraphs);
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
            .map(|p| p.paragraph.content.trim().to_string())
            .collect::<Vec<_>>()
            .join("\n\n");
        out.push(ChunkRecord::new(
            content,
            "sliding_window",
            paragraph_type_str(&window[0].paragraph.paragraph_type),
            window.iter().find_map(|p| p.paragraph.heading_level),
            context_of(window),
        ));

        if end == items.len() {
            break;
        }
        start += step;
    }

    number(&mut out);
    out
}

pub(crate) fn build_sentence_chunks(
    paragraphs: Vec<DocParagraph>,
    sentences_per_chunk: usize,
) -> Vec<ChunkRecord> {
    if sentences_per_chunk == 0 {
        return Vec::new();
    }

    let mut sentences: Vec<(String, Option<u8>, &'static str, ChunkContext)> = Vec::new();
    for item in position(paragraphs) {
        let p = &item.paragraph;
        if matches!(p.paragraph_type, ParagraphType::PageBreak) {
            continue;
        }
        let text = p.content.trim();
        if text.is_empty() {
            continue;
        }
        let mut parts = split_sentences(text);
        if parts.is_empty() {
            parts.push(text.to_string());
        }
        for s in parts {
            sentences.push((
                s,
                p.heading_level,
                paragraph_type_str(&p.paragraph_type),
                item.context.clone(),
            ));
        }
    }

    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < sentences.len() {
        let end = (idx + sentences_per_chunk).min(sentences.len());
        let window = &sentences[idx..end];
        let content = window
            .iter()
            .map(|(s, _, _, _)| s.clone())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(ChunkRecord::new(
            content,
            "sentence",
            window[0].2,
            window[0].1,
            // The sentence a chunk starts with fixes its position, the same
            // rule the paragraph-shaped builders apply.
            window[0].3.clone(),
        ));
        idx = end;
    }

    number(&mut out);
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
    let mut acc: Vec<Positioned> = Vec::new();

    let flush = |out: &mut Vec<ChunkRecord>, acc: &mut Vec<Positioned>| {
        if acc.is_empty() {
            return;
        }
        let content = acc
            .iter()
            .map(|p| p.paragraph.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !content.is_empty() {
            out.push(ChunkRecord::new(
                content,
                "page_aware",
                paragraph_type_str(&acc[0].paragraph.paragraph_type),
                acc.iter().find_map(|p| p.paragraph.heading_level),
                context_of(acc),
            ));
        }
        acc.clear();
    };

    for item in position(paragraphs) {
        if matches!(item.paragraph.paragraph_type, ParagraphType::PageBreak) {
            flush(&mut out, &mut acc);
            continue;
        }
        if item.paragraph.content.trim().is_empty() {
            continue;
        }
        acc.push(item);
        let normal_count = acc
            .iter()
            .filter(|x| matches!(x.paragraph.paragraph_type, ParagraphType::Normal))
            .count();
        if normal_count >= paragraphs_per_page {
            flush(&mut out, &mut acc);
        }
    }
    flush(&mut out, &mut acc);

    number(&mut out);
    out
}

#[cfg(test)]
mod page_provenance_tests {
    use super::*;

    /// A three-page document: two paragraphs per page, page breaks between.
    fn paged_paragraphs() -> Vec<DocParagraph> {
        let mut out = Vec::new();
        for page in 0..3usize {
            if page > 0 {
                out.push(DocParagraph::plain(
                    String::new(),
                    ParagraphType::PageBreak,
                    Some(page - 1),
                ));
            }
            out.push(DocParagraph::plain(
                format!("Heading for page {}", page + 1),
                ParagraphType::Heading(1),
                Some(page),
            ));
            out.push(DocParagraph::plain(
                // Long enough not to be swept into the short-paragraph buffer,
                // which would move the chunk's page to where the buffer began.
                format!(
                    "Body text for page {} that is comfortably longer than the \
                     eighty character threshold used to aggregate short paragraphs.",
                    page + 1
                ),
                ParagraphType::Normal,
                Some(page),
            ));
        }
        out
    }

    fn pages_of(records: &[ChunkRecord]) -> Vec<Option<usize>> {
        records.iter().map(|r| r.context.page_number).collect()
    }

    /// TECH_DEBT #11: every builder must carry page provenance through, not
    /// just `page_aware`. `records_to_chunks` used to hardcode null for all of
    /// them, so `.doc` and `.ppt` had no page or slide number anywhere.
    #[test]
    fn every_builder_reports_a_page_number() {
        let cases: Vec<(&str, Vec<ChunkRecord>)> = vec![
            ("structural", build_structural_chunks(paged_paragraphs())),
            ("section", build_section_chunks(paged_paragraphs())),
            ("semantic", build_semantic_chunks(paged_paragraphs())),
            ("sentence", build_sentence_chunks(paged_paragraphs(), 2)),
            ("page_aware", build_page_aware_chunks(paged_paragraphs(), 2)),
            (
                "sliding_window",
                build_sliding_window_chunks(paged_paragraphs(), 2, 1),
            ),
        ];

        for (mode, records) in cases {
            assert!(!records.is_empty(), "{mode}: expected chunks");
            let pages = pages_of(&records);
            assert!(
                pages.iter().all(Option::is_some),
                "{mode}: a chunk lost its page number: {pages:?}"
            );
            assert!(
                pages.iter().flatten().all(|p| (1..=3).contains(p)),
                "{mode}: page numbers outside the document's 3 pages: {pages:?}"
            );
            // Chunks are emitted in reading order, so pages never go backwards.
            let seq: Vec<usize> = pages.iter().flatten().copied().collect();
            assert!(
                seq.windows(2).all(|w| w[0] <= w[1]),
                "{mode}: page numbers are not monotonic: {seq:?}"
            );
        }
    }

    /// `page_aware` splits on the page breaks themselves, so its chunks map
    /// one-to-one onto the document's declared pages.
    #[test]
    fn page_aware_numbers_each_declared_page() {
        let records = build_page_aware_chunks(paged_paragraphs(), 50);
        assert_eq!(
            pages_of(&records),
            vec![Some(1), Some(2), Some(3)],
            "one chunk per declared page, numbered in order"
        );
    }

    /// A document with no provenance at all still produces chunks — the field
    /// is optional, not required.
    #[test]
    fn absent_provenance_stays_absent() {
        let paragraphs = vec![DocParagraph::plain(
            "A document that declares no page breaks at all.".to_string(),
            ParagraphType::Normal,
            None,
        )];
        let records = build_structural_chunks(paragraphs);
        assert_eq!(pages_of(&records), vec![None]);
    }
}

#[cfg(test)]
mod breadcrumb_tests {
    use super::*;

    fn body(n: u8) -> DocParagraph {
        DocParagraph::plain(
            format!(
                "Instructions about final paper and figure submission number {n} are \
                 given here at a length that keeps them out of the short-paragraph buffer."
            ),
            ParagraphType::Normal,
            None,
        )
    }

    /// Two sibling sections under one parent, so every builder — including the
    /// aggregating ones, whose first chunk legitimately starts at the top
    /// heading — has a chunk that begins inside the *second* section.
    fn document() -> Vec<DocParagraph> {
        vec![
            DocParagraph::plain("Procedure".into(), ParagraphType::Heading(1), None),
            DocParagraph::plain("Review Stage".into(), ParagraphType::Heading(2), None),
            body(1),
            DocParagraph::plain("Final Stage".into(), ParagraphType::Heading(2), None),
            body(2),
        ]
    }

    /// TECH_DEBT #12: every mode must carry the section trail, not just the
    /// mode that happens to track headings for its own purposes.
    #[test]
    fn every_builder_carries_the_section_breadcrumb() {
        let cases: Vec<(&str, Vec<ChunkRecord>)> = vec![
            ("structural", build_structural_chunks(document())),
            ("section", build_section_chunks(document())),
            ("semantic", build_semantic_chunks(document())),
            ("sentence", build_sentence_chunks(document(), 1)),
            ("page_aware", build_page_aware_chunks(document(), 1)),
            ("sliding_window", build_sliding_window_chunks(document(), 1, 0)),
        ];
        for (mode, records) in cases {
            let last = records.last().unwrap_or_else(|| panic!("{mode}: expected chunks"));
            assert_eq!(
                last.context.heading_path_string().as_deref(),
                Some("Procedure > Final Stage"),
                "{mode}: the last chunk must name the section it sits in"
            );
            assert_eq!(
                last.context.section_heading.as_deref(),
                Some("Final Stage"),
                "{mode}: section_heading is the innermost heading"
            );
        }
    }

    /// A chunk that starts on a list item reports how deep the item is nested.
    #[test]
    fn list_depth_reaches_the_chunk() {
        let mut nested = DocParagraph::plain(
            "A nested list item long enough to become a chunk of its own rather \
             than joining the short-paragraph buffer."
                .into(),
            ParagraphType::ListItem,
            None,
        );
        nested.list_level = Some(2);
        let records = build_structural_chunks(vec![nested]);
        assert_eq!(records[0].context.list_level, Some(2));
        assert_eq!(records[0].content_type, "bullet_list");
    }
}
