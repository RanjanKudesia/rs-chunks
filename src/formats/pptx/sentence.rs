use serde_json::json;

use super::common::{
    collect_slide_names, open_pptx, read_all_slides, split_sentences,
    ChunkRecordInput, ContentType,
};

pub fn build_sentence_chunks(bytes: &[u8], sentences_per_chunk: usize) -> Result<Vec<ChunkRecordInput>, String> {
    if sentences_per_chunk == 0 { return Err("sentences_per_chunk must be > 0".to_string()); }
    let mut archive = open_pptx(bytes)?;
    let slide_names = collect_slide_names(&archive);
    if slide_names.is_empty() { return Err("No slides found".to_string()); }
    let total_slides = slide_names.len();

    // Collect all sentences with their source slide number.
    // Title and body are split separately so a title ending with ':' cannot
    // fuse with the first body bullet.
    let mut all_sentences: Vec<(String, usize)> = Vec::new();
    for (slide_num, slide) in read_all_slides(&mut archive, &slide_names)? {
        // Treat the slide title as one standalone sentence unit.
        if let Some(ref title) = slide.title {
            let t = title.trim().to_string();
            if !t.is_empty() {
                all_sentences.push((t, slide_num));
            }
        }
        // Split each body paragraph independently.
        for body_para in &slide.body_paragraphs {
            let trimmed = body_para.trim();
            if !trimmed.is_empty() {
                for s in split_sentences(trimmed) {
                    if !s.is_empty() {
                        all_sentences.push((s, slide_num));
                    }
                }
            }
        }
    }

    if all_sentences.is_empty() { return Ok(Vec::new()); }

    let mut result: Vec<ChunkRecordInput> = Vec::new();
    let mut chunk_index = 0usize;
    let mut i = 0usize;
    while i < all_sentences.len() {
        let end = (i + sentences_per_chunk).min(all_sentences.len());
        let window = &all_sentences[i..end];
        let content = window.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(" ");
        if !content.is_empty() {
            result.push(ChunkRecordInput {
                content_type: ContentType::Sentence,
                content,
                metadata: json!({
                    "sentences_per_chunk":    sentences_per_chunk,
                    "actual_sentence_count":  window.len(),
                    "chunk_index":            chunk_index,
                    "source_slide":           window[0].1,
                    "slide_range":            [window[0].1, window.last().unwrap().1],
                    "document_metadata": { "source_type": "pptx", "total_slides": total_slides }
                }),
            });
            chunk_index += 1;
        }
        i = end;
    }

    if result.is_empty() { return Err("No sentence chunks generated".to_string()); }
    Ok(result)
}

