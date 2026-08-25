// Extract slide text from a legacy `.ppt` "PowerPoint Document" stream into the
// shared `DocParagraph` representation, so the entire `.doc` chunking, streaming
// and Markdown stack can be reused unchanged.
//
// Primary source is the Document container's SlideListWithText with
// recInstance == 0 (the *slides* list, in presentation order; instances 1 and 2
// are master / notes and are ignored). Each slide is delimited by a
// SlidePersistAtom (0x03F3); each text run is preceded by a TextHeaderAtom
// (0x0F9F) whose txType marks titles.
//
// On top of that:
//   * Body placeholders that split into multiple paragraphs are emitted as
//     ListItem (PowerPoint body placeholders are bulleted lists), giving real
//     Markdown bullets. Single-paragraph bodies stay prose.
//   * Freeform (non-placeholder) text boxes live only in each slide's drawing
//     (Escher 0xF00D); after building the SLWT slides we merge in any Escher
//     text not already present, deduplicated, so nothing is dropped.
//   * Slides are kept in SLWT (presentation) order; the Escher merge is matched
//     by slide index and skipped if the counts disagree, so it can never
//     misattribute text.
//
// A PageBreak paragraph is inserted between slides.

use crate::formats::doc::text_extractor::{DocParagraph, ParagraphType};

use super::records::{
    parse_header, read_txtype, REC_VER_CONTAINER, RT_MAIN_MASTER, RT_NOTES_CONTAINER,
    RT_SLIDE_CONTAINER, RT_SLIDE_LIST_WITH_TEXT, RT_SLIDE_PERSIST_ATOM, RT_TEXT_BYTES_ATOM,
    RT_TEXT_CHARS_ATOM, RT_TEXT_HEADER_ATOM, SLWT_INSTANCE_SLIDES, TX_TYPE_CENTER_TITLE,
    TX_TYPE_TITLE,
};

fn decode_utf16le(body: &[u8]) -> String {
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn decode_latin1(body: &[u8]) -> String {
    body.iter().map(|&b| b as char).collect()
}

/// Split a raw PowerPoint text run into clean paragraphs. `\r` (0x0D) ends a
/// paragraph; `\x0B` (vertical tab, a soft line break) becomes a space; other
/// control characters are dropped.
fn split_runs(raw: &str) -> Vec<String> {
    raw.split('\r')
        .map(|para| {
            para.chars()
                .map(|c| if c == '\u{000B}' { ' ' } else { c })
                .filter(|&c| c == '\t' || c == '\n' || !c.is_control())
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Drop placeholder noise: unedited master prompts, runs with no alphanumeric
/// characters (stray bullet glyphs), and short pure-digit runs (slide numbers).
fn is_noise_paragraph(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("click to edit") || lower.starts_with("click to add") {
        return true;
    }
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }
    if trimmed.len() <= 3 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    false
}

fn is_title_txtype(txtype: Option<u32>) -> bool {
    matches!(txtype, Some(TX_TYPE_TITLE) | Some(TX_TYPE_CENTER_TITLE))
}

/// Heuristic fallback for slide titles when txType is unavailable: short,
/// alphabetic, not ending like a sentence.
fn looks_like_title(text: &str) -> bool {
    let trimmed = text.trim();
    let words = trimmed.split_whitespace().count();
    if words == 0 || words > 12 {
        return false;
    }
    if trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?') {
        return false;
    }
    trimmed.chars().any(|c| c.is_alphabetic())
}

/// Normalize a paragraph for cross-source duplicate detection.
fn norm(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

struct Run {
    text: String,
    txtype: Option<u32>,
}

/// Per-slide paragraph lists, in presentation (SLWT) order when a central
/// slide list exists, otherwise in slide-drawing (Escher) order. This is the
/// pre-flattening view used by the image extractor to attribute images to
/// slides; `extract_paragraphs` flattens it.
pub fn extract_slides(stream: &[u8]) -> Vec<Vec<DocParagraph>> {
    // Per-slide paragraph lists from the slides SlideListWithText.
    let mut slides: Vec<Vec<DocParagraph>> = Vec::new();
    collect_slwt_slides(stream, 0, stream.len(), &mut slides);

    if slides.is_empty() {
        // No central slide list: fall back to walking slide drawings directly.
        let mut escher: Vec<Vec<String>> = Vec::new();
        collect_escher_slides(stream, 0, stream.len(), &mut escher);
        for raw in escher {
            slides.push(build_slide_paragraphs(&raw));
        }
    } else {
        // Merge in freeform text boxes (Escher) not already in the SLWT text.
        let mut escher: Vec<Vec<String>> = Vec::new();
        collect_escher_slides(stream, 0, stream.len(), &mut escher);
        if escher.len() == slides.len() {
            for (slide, raw) in slides.iter_mut().zip(escher.iter()) {
                merge_freeform(slide, raw);
            }
        }
    }
    slides
}

pub fn extract_paragraphs(stream: &[u8]) -> Vec<DocParagraph> {
    let slides = extract_slides(stream);

    // Flatten with PageBreak separators between non-empty slides.
    //
    // Each paragraph is stamped with its slide's **true** ordinal, taken from
    // this enumeration — which counts the text-free slides skipped just below.
    // Numbering by the emitted separators instead would shift every slide after
    // a text-free one: the same mistake TECH_DEBT #19 made in image attribution.
    let mut out: Vec<DocParagraph> = Vec::new();
    for (slide_index, mut slide) in slides.into_iter().enumerate() {
        if slide.is_empty() {
            continue;
        }
        for paragraph in &mut slide {
            paragraph.page_index = Some(slide_index);
        }
        out.extend(slide);
        out.push(DocParagraph::plain(
            String::new(),
            ParagraphType::PageBreak,
            Some(slide_index),
        ));
    }
    while matches!(
        out.last().map(|p| &p.paragraph_type),
        Some(ParagraphType::PageBreak)
    ) {
        out.pop();
    }
    out
}

/// Turn a slide's text runs into classified paragraphs (title / bullet / prose).
fn build_slide_from_runs(runs: &[Run]) -> Vec<DocParagraph> {
    let mut out: Vec<DocParagraph> = Vec::new();
    let mut slide_has_heading = false;

    for run in runs {
        let paras: Vec<String> = split_runs(&run.text)
            .into_iter()
            .filter(|p| !is_noise_paragraph(p))
            .collect();
        if paras.is_empty() {
            continue;
        }
        let title_run = is_title_txtype(run.txtype);
        // A body placeholder with multiple lines is a bullet list.
        let bullet_run = !title_run && paras.len() > 1;

        for (idx, content) in paras.into_iter().enumerate() {
            let paragraph_type = if title_run && idx == 0 {
                slide_has_heading = true;
                ParagraphType::Heading(2)
            } else if bullet_run {
                ParagraphType::ListItem
            } else if !slide_has_heading && out.is_empty() && looks_like_title(&content) {
                slide_has_heading = true;
                ParagraphType::Heading(2)
            } else {
                ParagraphType::Normal
            };
            out.push(DocParagraph::plain(content, paragraph_type, None));
        }
    }
    out
}

/// Build paragraphs from a flat list of raw slide text strings (Escher fallback).
fn build_slide_paragraphs(raw: &[String]) -> Vec<DocParagraph> {
    let runs: Vec<Run> = raw
        .iter()
        .map(|t| Run {
            text: t.clone(),
            txtype: None,
        })
        .collect();
    build_slide_from_runs(&runs)
}

/// Append Escher paragraphs whose normalized text is not already present.
fn merge_freeform(slide: &mut Vec<DocParagraph>, escher_raw: &[String]) {
    let mut seen: std::collections::HashSet<String> =
        slide.iter().map(|p| norm(&p.content)).collect();
    for raw in escher_raw {
        for para in split_runs(raw) {
            if is_noise_paragraph(&para) {
                continue;
            }
            let key = norm(&para);
            if key.is_empty() || seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            slide.push(DocParagraph::plain(para, ParagraphType::Normal, None));
        }
    }
}

/// Find the slides SlideListWithText (recInstance 0) and collect its slides.
fn collect_slwt_slides(data: &[u8], start: usize, end: usize, slides: &mut Vec<Vec<DocParagraph>>) {
    collect_slwt_slides_at(data, start, end, slides, 0)
}

/// Depth-capped worker. Container nesting costs 8 bytes a level, so an uncapped
/// descent is a stack-overflow abort (SIGABRT) from a small file — see
/// `odraw::MAX_RECORD_DEPTH`.
fn collect_slwt_slides_at(
    data: &[u8],
    start: usize,
    end: usize,
    slides: &mut Vec<Vec<DocParagraph>>,
    depth: usize,
) {
    let mut pos = start;
    while let Some((hdr, next)) = parse_header(data, pos, end) {
        if next <= pos {
            break;
        }
        if hdr.rec_type == RT_SLIDE_LIST_WITH_TEXT {
            if hdr.rec_instance == SLWT_INSTANCE_SLIDES {
                parse_slwt_children(data, hdr.body_start, hdr.body_end, slides);
            }
        } else if hdr.rec_ver == REC_VER_CONTAINER
            && depth < crate::formats::odraw::MAX_RECORD_DEPTH
        {
            collect_slwt_slides_at(data, hdr.body_start, hdr.body_end, slides, depth + 1);
        }
        pos = next;
    }
}

/// Iterate a SlideListWithText's children: SlidePersistAtom starts a slide,
/// TextHeaderAtom sets the current txType, text atoms accumulate.
fn parse_slwt_children(data: &[u8], start: usize, end: usize, slides: &mut Vec<Vec<DocParagraph>>) {
    let mut pos = start;
    let mut runs: Vec<Run> = Vec::new();
    let mut cur_txtype: Option<u32> = None;
    let mut started = false;

    let flush = |runs: &mut Vec<Run>, slides: &mut Vec<Vec<DocParagraph>>| {
        let slide = build_slide_from_runs(runs);
        runs.clear();
        if !slide.is_empty() {
            slides.push(slide);
        }
    };

    while let Some((hdr, next)) = parse_header(data, pos, end) {
        if next <= pos {
            break;
        }
        let body = &data[hdr.body_start..hdr.body_end];
        match hdr.rec_type {
            RT_SLIDE_PERSIST_ATOM => {
                if started {
                    flush(&mut runs, slides);
                }
                started = true;
                cur_txtype = None;
            }
            RT_TEXT_HEADER_ATOM => cur_txtype = read_txtype(body),
            RT_TEXT_CHARS_ATOM => runs.push(Run {
                text: decode_utf16le(body),
                txtype: cur_txtype,
            }),
            RT_TEXT_BYTES_ATOM => runs.push(Run {
                text: decode_latin1(body),
                txtype: cur_txtype,
            }),
            _ => {}
        }
        pos = next;
    }
    flush(&mut runs, slides);
}

/// Collect raw text per SlideContainer drawing (in document order).
fn collect_escher_slides(data: &[u8], start: usize, end: usize, out: &mut Vec<Vec<String>>) {
    collect_escher_slides_at(data, start, end, out, 0)
}

/// Depth-capped worker. Container nesting costs 8 bytes a level, so an uncapped
/// descent is a stack-overflow abort (SIGABRT) from a small file — see
/// `odraw::MAX_RECORD_DEPTH`.
fn collect_escher_slides_at(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut Vec<Vec<String>>,
    depth: usize,
) {
    let mut pos = start;
    while let Some((hdr, next)) = parse_header(data, pos, end) {
        if next <= pos {
            break;
        }
        match hdr.rec_type {
            RT_SLIDE_CONTAINER => {
                let mut raw: Vec<String> = Vec::new();
                collect_text_strings(data, hdr.body_start, hdr.body_end, &mut raw);
                out.push(raw);
            }
            RT_NOTES_CONTAINER | RT_MAIN_MASTER => {}
            _ => {
                if hdr.rec_ver == REC_VER_CONTAINER
                    && depth < crate::formats::odraw::MAX_RECORD_DEPTH
                {
                    collect_escher_slides_at(data, hdr.body_start, hdr.body_end, out, depth + 1);
                }
            }
        }
        pos = next;
    }
}

/// Recursively gather raw text-atom strings within a subtree.
fn collect_text_strings(data: &[u8], start: usize, end: usize, out: &mut Vec<String>) {
    collect_text_strings_at(data, start, end, out, 0)
}

/// Depth-capped worker. Container nesting costs 8 bytes a level, so an uncapped
/// descent is a stack-overflow abort (SIGABRT) from a small file — see
/// `odraw::MAX_RECORD_DEPTH`.
fn collect_text_strings_at(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut Vec<String>,
    depth: usize,
) {
    let mut pos = start;
    while let Some((hdr, next)) = parse_header(data, pos, end) {
        if next <= pos {
            break;
        }
        let body = &data[hdr.body_start..hdr.body_end];
        match hdr.rec_type {
            RT_TEXT_CHARS_ATOM => out.push(decode_utf16le(body)),
            RT_TEXT_BYTES_ATOM => out.push(decode_latin1(body)),
            _ => {
                if hdr.rec_ver == REC_VER_CONTAINER
                    && depth < crate::formats::odraw::MAX_RECORD_DEPTH
                {
                    collect_text_strings_at(data, hdr.body_start, hdr.body_end, out, depth + 1);
                }
            }
        }
        pos = next;
    }
}
