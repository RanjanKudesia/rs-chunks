/// Semantic chunker for plain text.
///
/// Prose paragraphs (plain, long, short) are merged by topic continuity using
/// ten signals in priority order.  Headings, code blocks, tables, and lists
/// are always emitted standalone — they are structurally atomic.
///
/// Heading detection is heuristic (ALL CAPS, setext underline, ATX #):
/// each detected heading resets the section context carried in metadata.
use serde_json::json;
use std::collections::{HashMap, HashSet};

use super::common::{
    current_section_heading, current_section_level, extract_heading_text, has_keyword_overlap,
    heading_level_txt, heading_path_strings, parse_txt_blocks, tokenize_keywords,
    update_heading_stack, ChunkRecordInput, ContentType,
};
use crate::shared::ci_starts_with;

// ── Signal word tables ────────────────────────────────────────────────────────

const TRANSITION_BREAKS: &[&str] = &[
    "however",
    "nevertheless",
    "in contrast",
    "on the other hand",
    "meanwhile",
    "conversely",
    "that said",
    "in summary",
    "to summarize",
    "to conclude",
    "in conclusion",
    "to wrap up",
    "overall",
    "in closing",
];
const REFERENCE_STARTS: &[&str] = &[
    "this ",
    "it ",
    "they ",
    "these ",
    "that ",
    "those ",
    "its ",
    "their ",
    "such ",
    "the above",
    "the following",
    "the latter",
    "the former",
];
const ELABORATION_STARTS: &[&str] = &[
    "additionally",
    "furthermore",
    "moreover",
    "in addition",
    "what is more",
    "on top of that",
    "notably",
    "importantly",
    "it is worth",
    "equally",
    "similarly",
    "likewise",
];
const EXAMPLE_STARTS: &[&str] = &[
    "for example",
    "for instance",
    "such as",
    "e.g.",
    "i.e.",
    "as an example",
    "to illustrate",
    "consider ",
    "as shown",
    "as demonstrated",
    "take ",
    "imagine ",
];
const CAUSE_EFFECT_STARTS: &[&str] = &[
    "because",
    "therefore",
    "thus",
    "hence",
    "as a result",
    "consequently",
    "this means",
    "this leads",
    "this causes",
    "this results",
    "this implies",
    "this suggests",
    "so ",
];
const CONTRAST_CONTINUATION: &[&str] = &[
    "although",
    "even though",
    "despite",
    "whereas",
    "even if",
    "regardless",
    "notwithstanding",
    "while it",
    "while this",
];

const MAX_SEMANTIC_CHARS: usize = 1500;
const SHORT_PARA_CHARS: usize = 80;

// ── Accumulator ───────────────────────────────────────────────────────────────

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
        }
    }
    fn push(&mut self, content: String, reason: &'static str) {
        self.char_count += content.len() + 2;
        self.ends_with_question = content.trim_end().ends_with('?');
        self.ends_with_definition_label = content.len() <= 80 && content.trim_end().ends_with(':');
        self.keywords.extend(tokenize_keywords(&content));
        self.parts.push(SemPart { content, reason });
    }
    fn finalize(self, chunk_index: usize, total: usize) -> ChunkRecordInput {
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
            "initial"
        } else {
            // Sort by (count desc, key asc) for determinism when counts are tied.
            let mut reason_vec: Vec<(&'static str, usize)> = counts.into_iter().collect();
            reason_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            reason_vec
                .first()
                .map(|(k, _)| *k)
                .unwrap_or("keyword_overlap")
        };
        let para_count = self.parts.len();
        let tw = content.split_whitespace().count().max(1);
        let kd = (self.keywords.len() as f64 / tw as f64 * 1000.0).round() / 1000.0;
        let avg_bl = if self.parts.is_empty() {
            0
        } else {
            self.parts.iter().map(|p| p.content.len()).sum::<usize>() / self.parts.len()
        };
        ChunkRecordInput {
            content_type: ContentType::Semantic,
            content,
            metadata: json!({
                "section_heading":      self.section_heading,
                "heading_path":         self.heading_path,
                "section_level":        self.section_level,
                "paragraph_count":      para_count,
                "merge_reasons":        merge_reasons,
                "primary_merge_reason": primary,
                "keyword_density":      kd,
                "avg_block_length":     avg_bl,
                "chunk_index":          chunk_index,
                "document_metadata": { "source_type": "txt", "total_input_blocks": total }
            }),
        }
    }
}

fn decide_merge(clean: &str, accum: &SemAccum) -> Option<&'static str> {
    if accum.char_count + clean.len() + 2 > MAX_SEMANTIC_CHARS {
        return None;
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
    if accum.ends_with_question {
        return Some("question_answer");
    }
    if accum.ends_with_definition_label && clean.len() > 60 {
        return Some("definition_expansion");
    }
    if clean.len() <= SHORT_PARA_CHARS {
        return Some("short_paragraph");
    }
    let bkw = tokenize_keywords(clean);
    if has_keyword_overlap(&accum.keywords, &bkw) {
        return Some("keyword_overlap");
    }
    None
}

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_semantic_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let text = crate::text_encoding::decode_text(bytes).0;
    if text.trim().is_empty() {
        return Err("TXT file is empty".to_string());
    }

    let blocks = parse_txt_blocks(&text);
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
            result.push(a.finalize(*ci, total));
            *ci += 1;
        }
    };

    for block in &blocks {
        let is_prose = matches!(
            block.content_type,
            ContentType::PlainParagraph
                | ContentType::LongSingleParagraph
                | ContentType::ShortDisconnectedParagraph
        );

        if block.content_type == ContentType::HeadingSection {
            flush(&mut accum, &mut result, &mut chunk_index, total);
            let level = heading_level_txt(&block.content);
            let text = extract_heading_text(&block.content);
            update_heading_stack(&mut heading_stack, level, text.clone());
            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection,
                content: text.clone(),
                metadata: json!({
                    "section_heading":      current_section_heading(&heading_stack[..heading_stack.len()-1]),
                    "heading_path":         heading_path_strings(&heading_stack),
                    "section_level":        level,
                    "paragraph_count":      0,
                    "merge_reasons":        [],
                    "primary_merge_reason": "initial",
                    "keyword_density":      0.0,
                    "avg_block_length":     text.len(),
                    "chunk_index":          chunk_index,
                    "document_metadata": { "source_type": "txt", "total_input_blocks": total }
                }),
            });
            chunk_index += 1;
        } else if !is_prose {
            // Code, table, list: standalone
            flush(&mut accum, &mut result, &mut chunk_index, total);
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
