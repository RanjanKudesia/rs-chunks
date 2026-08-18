use super::common::{
    classify_chunk, collect_slide_names, open_pptx, pptx_metadata, read_all_slides,
    split_large_text, ChunkRecordInput, MAX_CHUNK_CHARS, MIN_CHUNK_CHARS,
};

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_structural_chunks(bytes: &[u8]) -> Result<Vec<ChunkRecordInput>, String> {
    let mut archive = open_pptx(bytes)?;
    let slide_names = collect_slide_names(&archive);
    if slide_names.is_empty() {
        return Err("No slides found in PPTX archive".to_string());
    }
    let total_slides = slide_names.len();

    // (content, slide_num, slide_title)
    let mut raw: Vec<(String, usize, Option<String>)> = Vec::new();
    for (slide_num, slide) in read_all_slides(&mut archive, &slide_names)? {
        let full_text = slide.all_text();
        if full_text.is_empty() {
            continue;
        }
        let title = slide.title;
        if full_text.len() <= MAX_CHUNK_CHARS {
            raw.push((full_text, slide_num, title));
        } else {
            for part in split_large_text(&full_text, MAX_CHUNK_CHARS) {
                raw.push((part, slide_num, title.clone()));
            }
        }
    }

    if raw.is_empty() {
        return Ok(Vec::new()); // empty deck → empty chunk list (consistent with docx/txt/…)
    }

    // Merge short consecutive same-slide chunks.
    // Hard cap at MAX_CHUNK_CHARS to honour the documented per-chunk limit.
    let mut merged: Vec<(String, usize, Option<String>)> = Vec::new();
    for (text, slide_num, title) in raw {
        if let Some((prev_text, prev_slide, _)) = merged.last_mut() {
            if *prev_slide == slide_num && text.len() < MIN_CHUNK_CHARS {
                let candidate = format!("{prev_text}\n{text}").trim().to_string();
                if candidate.len() <= MAX_CHUNK_CHARS {
                    *prev_text = candidate;
                    continue;
                }
            }
        }
        merged.push((text, slide_num, title));
    }

    let result = merged
        .into_iter()
        .map(|(text, slide_num, title)| ChunkRecordInput {
            content_type: classify_chunk(&text),
            content: text,
            metadata: pptx_metadata(slide_num, title, None, total_slides),
        })
        .collect();

    Ok(result)
}
