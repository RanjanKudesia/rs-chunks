use serde_json::json;
use std::collections::{HashMap, HashSet};

use super::common::{
    current_section_heading, current_section_level, has_keyword_overlap, heading_path_strings,
    html_metadata_with_path, parse_html_blocks, remove_comments, tokenize_keywords,
    update_heading_stack, ChunkRecordInput, ContentType, HtmlBlockType,
};
use crate::shared::{
    ci_starts_with, CAUSE_EFFECT_STARTS, CONTRAST_CONTINUATION, ELABORATION_STARTS, EXAMPLE_STARTS,
    REFERENCE_STARTS, SHORT_PARA_CHARS, TRANSITION_BREAKS,
};

const MAX_SEMANTIC_CHARS: usize = 1500;
// Chunks shorter than this are UI artifacts (button labels, nav links, etc.)
const MIN_CHUNK_CONTENT_CHARS: usize = 20;

struct SemPart {
    content: String,
    reason: &'static str,
}
struct SemAccum {
    parts: Vec<SemPart>,
    section_heading: Option<String>,
    heading_path: Vec<String>,
    section_level: u8,
    keywords: HashSet<String>,
    char_count: usize,
    ends_with_question: bool,
    ends_with_definition_label: bool,
    // True when the first part is a heading; the next prose paragraph always
    // merges in, converting a heading singleton into a headed semantic chunk.
    starts_with_heading: bool,
}
impl SemAccum {
    fn new(
        content: String,
        sh: Option<String>,
        hp: Vec<String>,
        sl: u8,
        kws: HashSet<String>,
    ) -> Self {
        let cc = content.len();
        let ewq = content.trim_end().ends_with('?');
        let ewdl = content.len() <= 80 && content.trim_end().ends_with(':');
        SemAccum {
            parts: vec![SemPart {
                content,
                reason: "initial",
            }],
            section_heading: sh,
            heading_path: hp,
            section_level: sl,
            keywords: kws,
            char_count: cc,
            ends_with_question: ewq,
            ends_with_definition_label: ewdl,
            starts_with_heading: false,
        }
    }
    fn new_heading(content: String, sh: Option<String>, hp: Vec<String>, sl: u8) -> Self {
        let cc = content.len();
        SemAccum {
            parts: vec![SemPart {
                content,
                reason: "initial",
            }],
            section_heading: sh,
            heading_path: hp,
            section_level: sl,
            keywords: HashSet::new(),
            char_count: cc,
            ends_with_question: false,
            ends_with_definition_label: false,
            starts_with_heading: true,
        }
    }
    fn push(&mut self, content: String, reason: &'static str) {
        self.char_count += content.len() + 2;
        self.ends_with_question = content.trim_end().ends_with('?');
        self.ends_with_definition_label = content.len() <= 80 && content.trim_end().ends_with(':');
        self.keywords.extend(tokenize_keywords(&content));
        self.parts.push(SemPart { content, reason });
    }
    fn finalize(self, ci: usize, total: usize) -> ChunkRecordInput {
        let is_heading_only = self.starts_with_heading && self.parts.len() == 1;
        let content = self
            .parts
            .iter()
            .map(|p| p.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut merge_reasons: Vec<&'static str> = Vec::new();
        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        for p in &self.parts {
            if p.reason != "initial" {
                if !merge_reasons.contains(&p.reason) {
                    merge_reasons.push(p.reason);
                }
                *counts.entry(p.reason).or_default() += 1;
            }
        }
        let primary = if self.parts.len() <= 1 {
            if self.starts_with_heading {
                "heading"
            } else {
                "initial"
            }
        } else {
            // Sort by (count desc, key asc) for determinism when counts are tied.
            let mut reason_vec: Vec<(&'static str, usize)> = counts.into_iter().collect();
            reason_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            reason_vec
                .first()
                .map(|(k, _)| *k)
                .unwrap_or("keyword_overlap")
        };
        let tw = content.split_whitespace().count().max(1);
        let kd = (self.keywords.len() as f64 / tw as f64 * 1000.0).round() / 1000.0;
        let content_type = if is_heading_only {
            ContentType::HeadingSection
        } else {
            ContentType::Semantic
        };
        ChunkRecordInput {
            content_type,
            content,
            metadata: json!({
                "section_heading": self.section_heading,
                "heading_path": self.heading_path,
                "section_level": self.section_level,
                "paragraph_count": self.parts.len(),
                "merge_reasons": merge_reasons,
                "primary_merge_reason": primary,
                // DOCX-compatible alias so callers can use a single key name
                "merge_reason": primary,
                "keyword_density": kd,
                "chunk_index": ci,
                "has_body_content": !is_heading_only,
                "document_metadata": { "source_type": "html", "total_input_blocks": total }
            }),
        }
    }
}

fn decide_merge(clean: &str, acc: &SemAccum) -> Option<&'static str> {
    if acc.char_count + clean.len() + 2 > MAX_SEMANTIC_CHARS {
        return None;
    }
    // Always merge the first prose paragraph after a heading, regardless of
    // transition words — a heading with zero body is nearly useless.
    if acc.starts_with_heading && acc.parts.len() == 1 {
        return Some("heading_merge");
    }
    let t = clean.trim_start();
    if TRANSITION_BREAKS.iter().any(|s| ci_starts_with(t, s)) {
        return None;
    }
    if REFERENCE_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("reference_continuity");
    }
    if ELABORATION_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("elaboration");
    }
    if EXAMPLE_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("example");
    }
    if CAUSE_EFFECT_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("cause_effect");
    }
    if CONTRAST_CONTINUATION.iter().any(|s| ci_starts_with(t, s)) {
        return Some("contrast_continuation");
    }
    if acc.ends_with_question {
        return Some("question_answer");
    }
    if acc.ends_with_definition_label && clean.len() > 60 {
        return Some("definition_expansion");
    }
    if clean.len() <= SHORT_PARA_CHARS {
        return Some("short_paragraph");
    }
    let bkw = tokenize_keywords(clean);
    if has_keyword_overlap(&acc.keywords, &bkw) {
        return Some("keyword_overlap");
    }
    None
}

fn emit_chunk(chunk: ChunkRecordInput, result: &mut Vec<ChunkRecordInput>, ci: &mut usize) {
    // Keep all heading chunks (even empty-section ones); filter UI-artifact prose
    if chunk.content_type == ContentType::HeadingSection
        || chunk.content.trim().len() >= MIN_CHUNK_CONTENT_CHARS
    {
        result.push(chunk);
        *ci += 1;
    }
}

pub fn build_semantic_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let text = super::encoding::decode_html(bytes);
    if text.trim().is_empty() {
        return Err("HTML file is empty".to_string());
    }
    let blocks = parse_html_blocks(&remove_comments(&text));
    let total = blocks.len();
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut accum: Option<SemAccum> = None;
    let mut chunk_index = 0usize;

    let flush = |accum: &mut Option<SemAccum>,
                 result: &mut Vec<ChunkRecordInput>,
                 ci: &mut usize,
                 total: usize| {
        if let Some(a) = accum.take() {
            let chunk = a.finalize(*ci, total);
            emit_chunk(chunk, result, ci);
        }
    };

    let is_prose = |bt: HtmlBlockType| matches!(bt, HtmlBlockType::Paragraph);

    for block in &blocks {
        if block.block_type == HtmlBlockType::Heading {
            flush(&mut accum, &mut result, &mut chunk_index, total);
            update_heading_stack(
                &mut heading_stack,
                block.heading_level,
                block.content.clone(),
            );
            // Start a heading accumulator. The next prose paragraph will fold in
            // via "heading_merge", turning this into a headed semantic chunk.
            // If no prose follows (another heading or block comes first), it
            // gets emitted as a standalone HeadingSection on the next flush.
            let parent_heading = current_section_heading(&heading_stack[..heading_stack.len() - 1]);
            accum = Some(SemAccum::new_heading(
                block.content.clone(),
                parent_heading,
                heading_path_strings(&heading_stack),
                block.heading_level,
            ));
        } else if !is_prose(block.block_type) {
            flush(&mut accum, &mut result, &mut chunk_index, total);
            let ct = match block.block_type {
                HtmlBlockType::Code => ContentType::CodeBlock,
                HtmlBlockType::Table => ContentType::Table,
                HtmlBlockType::List => ContentType::BulletNumberedList,
                _ => ContentType::PlainParagraph,
            };
            let chunk = ChunkRecordInput {
                content_type: ct,
                content: block.content.clone(),
                metadata: html_metadata_with_path(
                    current_section_heading(&heading_stack),
                    heading_path_strings(&heading_stack),
                    current_section_level(&heading_stack),
                ),
            };
            emit_chunk(chunk, &mut result, &mut chunk_index);
        } else {
            let clean = block.content.trim().to_string();
            if clean.is_empty() {
                continue;
            }
            let bkws = tokenize_keywords(&clean);
            match accum.as_mut() {
                None => {
                    accum = Some(SemAccum::new(
                        clean,
                        current_section_heading(&heading_stack),
                        heading_path_strings(&heading_stack),
                        current_section_level(&heading_stack),
                        bkws,
                    ));
                }
                Some(a) => match decide_merge(&clean, a) {
                    Some(reason) => {
                        a.push(clean, reason);
                    }
                    None => {
                        flush(&mut accum, &mut result, &mut chunk_index, total);
                        accum = Some(SemAccum::new(
                            clean,
                            current_section_heading(&heading_stack),
                            heading_path_strings(&heading_stack),
                            current_section_level(&heading_stack),
                            bkws,
                        ));
                    }
                },
            }
        }
    }
    flush(&mut accum, &mut result, &mut chunk_index, total);
    // Empty is not a failure (TECH_DEBT T6): the document parsed, this mode
    // simply produced nothing. Returning `[]` keeps every mode consistent with
    // docx/ppt/xlsx and lets epub distinguish an empty chapter from a broken
    // one without swallowing errors (L14).
    if result.is_empty() {
        return Ok(Vec::new());
    }
    Ok(result)
}
