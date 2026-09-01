/// Semantic chunker for Markdown.
///
/// Groups typed blocks (paragraphs, lists) by topic continuity using ten
/// distinct signals, in strict priority order.  Code blocks and tables are
/// always emitted as standalone chunks because they are structurally atomic.
/// Headings are always standalone and reset the heading-path context.
///
/// Each output chunk carries rich metadata: full heading breadcrumb, every
/// distinct merge reason that fired, the dominant reason, block-type
/// inventory, keyword density, and per-chunk position.
use serde_json::json;
use std::collections::{HashMap, HashSet};

use super::common::{
    current_section_heading, current_section_level, extend_span, extract_heading_text,
    has_keyword_overlap, heading_level, heading_path_strings, parse_markdown_blocks,
    strip_block_content, tokenize_keywords, update_heading_stack, BlockSpan, ChunkRecordInput,
    ContentType, MdBlockType, SpannedRecord,
};
use crate::shared::{
    ci_starts_with, CAUSE_EFFECT_STARTS, CONTRAST_CONTINUATION, ELABORATION_STARTS, EXAMPLE_STARTS,
    MAX_SEMANTIC_CHARS, REFERENCE_STARTS, TRANSITION_BREAKS,
};

// ── Signal word tables ────────────────────────────────────────────────────────

// Signal word tables — imported from crate::shared (single source of truth).
// See shared.rs for the full definitions and comments on each signal.

// ── Internal accumulator ──────────────────────────────────────────────────────

struct SemanticPart {
    content: String,
    block_type: MdBlockType,
    merge_reason: &'static str,
}

struct SemanticAccum {
    parts: Vec<SemanticPart>,
    /// The blocks this accumulator's parts came from.
    span: Option<BlockSpan>,
    section_heading: Option<String>,
    heading_path: Vec<String>,
    section_level: u8,
    /// Accumulated significant keywords across all merged blocks.
    keywords: HashSet<String>,
    /// Total character count of joined content.
    char_count: usize,
    /// Whether the last emitted paragraph ends with a question mark.
    ends_with_question: bool,
    /// Whether the last emitted paragraph is a short definition-like line
    /// (≤ 80 chars, ends with ':').  Signals the next block may expand it.
    ends_with_definition_label: bool,
}

impl SemanticAccum {
    fn new(
        first_content: String,
        first_type: MdBlockType,
        section_heading: Option<String>,
        heading_path: Vec<String>,
        section_level: u8,
        keywords: HashSet<String>,
        block: usize,
    ) -> Self {
        let char_count = first_content.len();
        let ends_with_question = first_content.trim_end().ends_with('?');
        let ends_with_definition_label =
            first_content.len() <= 80 && first_content.trim_end().ends_with(':');
        SemanticAccum {
            parts: vec![SemanticPart {
                content: first_content,
                block_type: first_type,
                merge_reason: "initial",
            }],
            span: Some((block, block)),
            section_heading,
            heading_path,
            section_level,
            keywords,
            char_count,
            ends_with_question,
            ends_with_definition_label,
        }
    }

    fn append(
        &mut self,
        content: String,
        block_type: MdBlockType,
        reason: &'static str,
        block: usize,
    ) {
        self.char_count += content.len() + 2;
        self.ends_with_question = content.trim_end().ends_with('?');
        self.ends_with_definition_label = content.len() <= 80 && content.trim_end().ends_with(':');
        self.keywords.extend(tokenize_keywords(&content));
        self.parts.push(SemanticPart {
            content,
            block_type,
            merge_reason: reason,
        });
        extend_span(&mut self.span, block);
    }

    fn joined_content(&self) -> String {
        self.parts
            .iter()
            .map(|p| p.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

// ── Merge decision ────────────────────────────────────────────────────────────

/// Returns `Some(reason)` if `clean` should be merged into `accum`, or `None`
/// if it must start a new chunk.
///
/// Signals are evaluated in strict priority order:
///   1. transition_break      — hard stop, always new chunk
///   2. reference_continuity  — pronoun/demonstrative continuing same subject
///   3. elaboration           — "furthermore", "additionally", etc.
///   4. example               — "for example", "e.g.", etc.
///   5. cause_effect          — "therefore", "as a result", etc.
///   6. contrast_continuation — "although", "despite", etc. (same topic)
///   7. question_answer       — previous paragraph was a question
///   8. definition_expansion  — previous was a short label ending with ':'
///   9. short_paragraph       — absorb prose ≤ 80 chars
///  10. list_continuation     — list shares keywords with preceding prose
///  11. keyword_overlap       — at least one shared significant keyword
fn decide_merge(
    clean: &str,
    block_type: MdBlockType,
    accum: &SemanticAccum,
    max_chars: usize,
) -> Option<&'static str> {
    // Hard size limit — never exceed regardless of signal.
    if accum.char_count + clean.len() + 2 > max_chars {
        return None;
    }

    let t = clean.trim_start();

    // 1. Transition break: topic has genuinely shifted.
    if TRANSITION_BREAKS.iter().any(|s| ci_starts_with(t, s)) {
        return None;
    }

    // 2. Reference continuity.
    if REFERENCE_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("reference_continuity");
    }

    // 3. Elaboration.
    if ELABORATION_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("elaboration");
    }

    // 4. Example.
    if EXAMPLE_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("example");
    }

    // 5. Cause / effect.
    if CAUSE_EFFECT_STARTS.iter().any(|s| ci_starts_with(t, s)) {
        return Some("cause_effect");
    }

    // 6. In-clause contrast (same topic, opposite framing).
    if CONTRAST_CONTINUATION.iter().any(|s| ci_starts_with(t, s)) {
        return Some("contrast_continuation");
    }

    // 7. Question → answer pair.
    if accum.ends_with_question {
        return Some("question_answer");
    }

    // 8. Definition label expansion: prev was "Term:" and this explains it.
    if accum.ends_with_definition_label && clean.len() > 60 {
        return Some("definition_expansion");
    }

    // 9. Short paragraph — absorb rather than emit a tiny standalone chunk.
    if clean.len() <= 80 {
        return Some("short_paragraph");
    }

    // 10+11. Keyword-based signals — compute keywords once, reuse for both checks.
    let bkw = tokenize_keywords(clean);
    if matches!(block_type, MdBlockType::List) && has_keyword_overlap(&accum.keywords, &bkw) {
        return Some("list_continuation");
    }

    // 11. Keyword overlap — paragraph shares at least one significant term.
    if has_keyword_overlap(&accum.keywords, &bkw) {
        return Some("keyword_overlap");
    }

    None
}

// ── Finalise an accumulator into a ChunkRecordInput ───────────────────────────

fn finalize(accum: SemanticAccum, chunk_index: usize, total_input_blocks: usize) -> SpannedRecord {
    let span = accum.span;
    let content = accum.joined_content();

    // Unique block types present in this chunk.
    let mut block_types: Vec<&'static str> = Vec::new();
    for part in &accum.parts {
        let t = match part.block_type {
            MdBlockType::Paragraph => "paragraph",
            MdBlockType::List => "list",
            MdBlockType::Code => "code_block",
            MdBlockType::Table => "table",
            MdBlockType::Heading => "heading",
        };
        if !block_types.contains(&t) {
            block_types.push(t);
        }
    }

    // All distinct merge reasons (excluding "initial").
    let mut merge_reasons: Vec<&'static str> = Vec::new();
    for part in &accum.parts {
        if part.merge_reason != "initial" && !merge_reasons.contains(&part.merge_reason) {
            merge_reasons.push(part.merge_reason);
        }
    }

    // Primary reason = the one that fired most often.
    let primary_merge_reason: &'static str = if accum.parts.len() <= 1 {
        "initial"
    } else {
        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        for part in &accum.parts {
            if part.merge_reason != "initial" {
                *counts.entry(part.merge_reason).or_default() += 1;
            }
        }
        // Sort by (count desc, key asc) for determinism when counts are tied.
        let mut reason_vec: Vec<(&'static str, usize)> = counts.into_iter().collect();
        reason_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        reason_vec
            .first()
            .map(|(r, _)| *r)
            .unwrap_or("keyword_overlap")
    };

    let has_list = accum
        .parts
        .iter()
        .any(|p| matches!(p.block_type, MdBlockType::List));

    let paragraph_count = accum
        .parts
        .iter()
        .filter(|p| matches!(p.block_type, MdBlockType::Paragraph | MdBlockType::List))
        .count();

    // Average block length (chars) before joining.
    let avg_block_length = if accum.parts.is_empty() {
        0
    } else {
        accum.parts.iter().map(|p| p.content.len()).sum::<usize>() / accum.parts.len()
    };

    // Keyword density = unique significant keywords / total word count.
    let total_words = content.split_whitespace().count().max(1);
    let keyword_density =
        (accum.keywords.len() as f64 / total_words as f64 * 1000.0).round() / 1000.0;

    let metadata = json!({
        "section_heading":         accum.section_heading,
        "heading_path":            accum.heading_path,
        "section_level":           accum.section_level,
        "paragraph_count":         paragraph_count,
        "block_types":             block_types,
        "merge_reasons":           merge_reasons,
        "primary_merge_reason":    primary_merge_reason,
        "has_list":                has_list,
        "keyword_density":         keyword_density,
        "avg_block_length":        avg_block_length,
        "chunk_index":             chunk_index,
        "document_metadata": {
            "source_type":         "md",
            "total_input_blocks":  total_input_blocks,
        }
    });

    SpannedRecord::spanning(
        ChunkRecordInput {
            content_type: ContentType::Semantic,
            content,
            metadata,
        },
        span,
    )
}

// ── Core algorithm ────────────────────────────────────────────────────────────

pub fn build_semantic_chunks(bytes: &[u8]) -> Result<Vec<SpannedRecord>, String> {
    let text = super::common::decode_body(bytes);

    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_markdown_blocks(&text);
    let total_input_blocks = blocks.len();

    let mut result: Vec<SpannedRecord> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut accum: Option<SemanticAccum> = None;
    let mut chunk_index = 0usize;

    for block in blocks {
        match block.block_type {
            // ── Headings: flush current group, emit heading standalone ────────
            MdBlockType::Heading => {
                if let Some(a) = accum.take() {
                    result.push(finalize(a, chunk_index, total_input_blocks));
                    chunk_index += 1;
                }

                let level = heading_level(&block.content);
                let text = extract_heading_text(&block.content);
                update_heading_stack(&mut heading_stack, level, text.clone());

                // Emit the heading itself as a typed heading chunk.
                result.push(SpannedRecord::at(ChunkRecordInput {
                    content_type: ContentType::HeadingSection,
                    content: text.clone(),
                    metadata: json!({
                        "section_heading":      current_section_heading(&heading_stack[..heading_stack.len()-1]),
                        "heading_path":         heading_path_strings(&heading_stack),
                        "section_level":        level,
                        "paragraph_count":      0,
                        "block_types":          ["heading"],
                        "merge_reasons":        [],
                        "primary_merge_reason": "initial",
                        "has_list":             false,
                        "keyword_density":      0.0,
                        "avg_block_length":     text.len(),
                        "chunk_index":          chunk_index,
                        "document_metadata": {
                            "source_type":        "md",
                            "total_input_blocks": total_input_blocks,
                        }
                    }),
                }, block.index));
                chunk_index += 1;
            }

            // ── Code blocks: always standalone ────────────────────────────────
            MdBlockType::Code => {
                if let Some(a) = accum.take() {
                    result.push(finalize(a, chunk_index, total_input_blocks));
                    chunk_index += 1;
                }
                let content = block.content.clone();
                result.push(SpannedRecord::at(
                    ChunkRecordInput {
                        content_type: ContentType::CodeBlock,
                        content: content.clone(),
                        metadata: json!({
                            "section_heading":      current_section_heading(&heading_stack),
                            "heading_path":         heading_path_strings(&heading_stack),
                            "section_level":        current_section_level(&heading_stack),
                            "paragraph_count":      0,
                            "block_types":          ["code_block"],
                            "merge_reasons":        [],
                            "primary_merge_reason": "structural_boundary",
                            "has_list":             false,
                            "keyword_density":      0.0,
                            "avg_block_length":     content.len(),
                            "chunk_index":          chunk_index,
                            "document_metadata": {
                                "source_type":        "md",
                                "total_input_blocks": total_input_blocks,
                            }
                        }),
                    },
                    block.index,
                ));
                chunk_index += 1;
            }

            // ── Tables: always standalone ─────────────────────────────────────
            MdBlockType::Table => {
                if let Some(a) = accum.take() {
                    result.push(finalize(a, chunk_index, total_input_blocks));
                    chunk_index += 1;
                }
                let content = block.content.clone();
                result.push(SpannedRecord::at(
                    ChunkRecordInput {
                        content_type: ContentType::Table,
                        content: content.clone(),
                        metadata: json!({
                            "section_heading":      current_section_heading(&heading_stack),
                            "heading_path":         heading_path_strings(&heading_stack),
                            "section_level":        current_section_level(&heading_stack),
                            "paragraph_count":      0,
                            "block_types":          ["table"],
                            "merge_reasons":        [],
                            "primary_merge_reason": "structural_boundary",
                            "has_list":             false,
                            "keyword_density":      0.0,
                            "avg_block_length":     content.len(),
                            "chunk_index":          chunk_index,
                            "document_metadata": {
                                "source_type":        "md",
                                "total_input_blocks": total_input_blocks,
                            }
                        }),
                    },
                    block.index,
                ));
                chunk_index += 1;
            }

            // ── Paragraphs and lists: apply full signal pipeline ───────────────
            MdBlockType::Paragraph | MdBlockType::List => {
                let clean = strip_block_content(
                    &block.content,
                    matches!(block.block_type, MdBlockType::List),
                );
                if clean.is_empty() {
                    continue;
                }

                match accum.as_mut() {
                    None => {
                        // Start the first accumulator for this section.
                        accum = Some(SemanticAccum::new(
                            clean.clone(),
                            block.block_type,
                            current_section_heading(&heading_stack),
                            heading_path_strings(&heading_stack),
                            current_section_level(&heading_stack),
                            tokenize_keywords(&clean),
                            block.index,
                        ));
                    }
                    Some(a) => {
                        match decide_merge(&clean, block.block_type, a, MAX_SEMANTIC_CHARS) {
                            Some(reason) => {
                                a.append(clean, block.block_type, reason, block.index);
                            }
                            None => {
                                // Flush current, start fresh.
                                let finished = accum.take().unwrap();
                                result.push(finalize(finished, chunk_index, total_input_blocks));
                                chunk_index += 1;
                                accum = Some(SemanticAccum::new(
                                    clean.clone(),
                                    block.block_type,
                                    current_section_heading(&heading_stack),
                                    heading_path_strings(&heading_stack),
                                    current_section_level(&heading_stack),
                                    tokenize_keywords(&clean),
                                    block.index,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Flush whatever remains.
    if let Some(a) = accum.take() {
        result.push(finalize(a, chunk_index, total_input_blocks));
    }

    // A document that parsed cleanly but holds no extractable text is EMPTY,
    // not broken: return an empty list rather than an error. Raising here
    // conflated "nothing to chunk" with "could not read this file", and it was
    // inconsistent — docx/ppt/xlsx already returned `[]` for the same condition
    // while txt/html/md raised (TECH_DEBT T6). Structural invalidity still
    // errors: a PPTX with no slides, or a PDF with no text layer, both raise a
    // typed error carrying a remedy.
    Ok(result)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────
