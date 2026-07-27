/// Section chunker for PPTX.
///
/// Detection priority:
///   1. Named sections from `ppt/presentation.xml` (p14:sectionLst) — present
///      in PowerPoint files that use the Sections feature.
///   2. Section-divider slides — slides with a title but no body text; these
///      act as section boundaries and everything after them groups together.
///   3. Fallback — treat each slide as its own section (same as structural).
///
/// Output schema: standard { content, content_type, metadata }.
use serde_json::json;

use super::common::{
    classify_chunk, collect_slide_names, open_pptx,
    parse_presentation_sections, read_all_slides, split_large_text, ChunkRecordInput, ContentType,
    SlideContent,
};

const MAX_SECTION_CHARS: usize = 2000;

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_section_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let mut archive = open_pptx(bytes)?;
    let slide_names = collect_slide_names(&archive);
    if slide_names.is_empty() {
        return Err("No slides found in PPTX archive".to_string());
    }
    let total_slides = slide_names.len();

    // Read all slides.
    let slides = read_all_slides(&mut archive, &slide_names)?;

    // Try named sections from presentation.xml.
    let named_sections = parse_presentation_sections(&mut archive).unwrap_or_default();

    let result = if !named_sections.is_empty() {
        build_from_named_sections(slides, named_sections, total_slides)
    } else {
        build_from_divider_heuristic(slides, total_slides)
    };

    if result.is_empty() {
        return Err("No section chunks generated from PPTX".to_string());
    }
    Ok(result)
}

fn flush_section_body(
    result: &mut Vec<ChunkRecordInput>,
    section_name: &str,
    slide_texts: &[(usize, String, Option<String>)],
    total_slides: usize,
) {
    if slide_texts.is_empty() {
        return;
    }
    let combined = slide_texts
        .iter()
        .map(|(_, t, _)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let combined = combined.trim().to_string();
    if combined.is_empty() {
        return;
    }
    let first_slide = slide_texts[0].0;
    let last_slide = slide_texts.last().unwrap().0;
    let parts = if combined.len() > MAX_SECTION_CHARS {
        split_large_text(&combined, MAX_SECTION_CHARS)
    } else {
        vec![combined.clone()]
    };
    let part_count = parts.len();
    for (i, content) in parts.into_iter().enumerate() {
        result.push(ChunkRecordInput {
            content_type: ContentType::Section,
            content: content.clone(),
            metadata: json!({
                "section_heading":  section_name,
                "slide_range":      [first_slide, last_slide],
                "slide_count":      slide_texts.len(),
                "split_part":       if part_count > 1 { json!(i+1) } else { serde_json::Value::Null },
                "split_total":      if part_count > 1 { json!(part_count) } else { serde_json::Value::Null },
                "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
            }),
        });
    }
}

fn build_from_named_sections(
    slides: Vec<(usize, SlideContent)>,
    sections: Vec<(String, Vec<usize>)>,
    total_slides: usize,
) -> Vec<ChunkRecordInput> {
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    for (section_name, positions) in sections {
        let pos_set: std::collections::HashSet<usize> = positions.into_iter().collect();
        let section_slides: Vec<(usize, String, Option<String>)> = slides
            .iter()
            .filter(|(n, _)| pos_set.contains(n))
            .map(|(n, s)| {
                let text = s.all_text();
                (*n, text, s.title.clone())
            })
            .filter(|(_, t, _)| !t.is_empty())
            .collect();

        // Only emit section heading chunk when there are non-empty slides to back it.
        if section_slides.is_empty() {
            continue;
        }

        // Emit section heading chunk.
        result.push(ChunkRecordInput {
            content_type: ContentType::HeadingSection,
            content: section_name.clone(),
            metadata: json!({
                "section_heading":  section_name,
                "slide_range":      [section_slides.first().map(|(n,_,_)| *n).unwrap_or(0),
                                     section_slides.last().map(|(n,_,_)| *n).unwrap_or(0)],
                "slide_count":      section_slides.len(),
                "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
            }),
        });

        flush_section_body(&mut result, &section_name, &section_slides, total_slides);
    }
    result
}

fn build_from_divider_heuristic(
    slides: Vec<(usize, SlideContent)>,
    total_slides: usize,
) -> Vec<ChunkRecordInput> {
    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut current_section: Option<String> = None;
    let mut current_body: Vec<(usize, String, Option<String>)> = Vec::new();

    let flush = |result: &mut Vec<ChunkRecordInput>,
                 section: &Option<String>,
                 body: &mut Vec<(usize, String, Option<String>)>,
                 total: usize| {
        if let Some(name) = section {
            flush_section_body(result, name, body, total);
        } else if !body.is_empty() {
            // Use the first slide's title as the section name instead of a hard-coded string.
            let intro_name = body
                .first()
                .and_then(|(_, _, t)| t.as_ref().map(|s| s.as_str()))
                .unwrap_or("Introduction")
                .to_string();
            flush_section_body(result, &intro_name, body, total);
        }
        body.clear();
    };

    for (slide_num, slide) in &slides {
        if slide.is_section_divider() {
            flush(
                &mut result,
                &current_section,
                &mut current_body,
                total_slides,
            );
            let heading = slide.title.clone().unwrap_or_default();
            current_section = Some(heading.clone());
            result.push(ChunkRecordInput {
                content_type: ContentType::HeadingSection,
                content: heading.clone(),
                metadata: json!({
                    "section_heading":   heading,
                    "slide_range":       [slide_num, slide_num],
                    "slide_count":       0,
                    "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
                }),
            });
        } else {
            let text = slide.all_text();
            if !text.is_empty() {
                current_body.push((*slide_num, text, slide.title.clone()));
            }
        }
    }
    flush(
        &mut result,
        &current_section,
        &mut current_body,
        total_slides,
    );

    // If no section dividers were found, produce one chunk per slide.
    if result.is_empty()
        || result
            .iter()
            .all(|c| c.content_type == ContentType::HeadingSection)
    {
        result.clear();
        for (slide_num, slide) in &slides {
            let text = slide.all_text();
            if text.is_empty() {
                continue;
            }
            for content in split_large_text(&text, MAX_SECTION_CHARS) {
                result.push(ChunkRecordInput {
                    content_type: classify_chunk(&content),
                    content: content.clone(),
                    metadata: json!({
                        "section_heading":   slide.title,
                        "slide_range":       [slide_num, slide_num],
                        "slide_count":       1,
                        "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
                    }),
                });
            }
        }
    }

    result
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────

