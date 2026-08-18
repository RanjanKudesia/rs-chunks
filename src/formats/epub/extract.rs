//! EPUB → chunks/markdown by walking the spine (reading order) and reusing the
//! mature HTML chunker on each content document.

use serde_json::json;

use super::package::EpubPackage;
use crate::formats::html::common::ChunkRecordInput;
use crate::formats::html::page_aware::build_page_aware_chunks;
use crate::formats::html::section::build_section_chunks;
use crate::formats::html::semantic::build_semantic_chunks;
use crate::formats::html::sentence::build_sentence_chunks;
use crate::formats::html::sliding_window::build_sliding_window_chunks;
use crate::formats::html::structural::build_chunks_from_html_bytes;
use crate::formats::html::to_markdown::html_to_markdown_str;

fn build_for_mode(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    match mode {
        "default" | "structural" => build_chunks_from_html_bytes(bytes),
        "section" => build_section_chunks(bytes),
        "semantic" => build_semantic_chunks(bytes),
        "sentence" => build_sentence_chunks(bytes, sentences_per_chunk),
        "page_aware" => build_page_aware_chunks(bytes, paragraphs_per_page),
        "sliding_window" => build_sliding_window_chunks(bytes, window_size, overlap),
        other => Err(format!("Unknown EPUB mode: {other}")),
    }
}

fn doc_metadata(pkg: &EpubPackage) -> serde_json::Value {
    json!({
        "source_type": "epub",
        "title": pkg.title,
        "language": pkg.language,
        "creator": pkg.creator,
        "identifier": pkg.identifier,
        "epub_version": pkg.version,
        "spine_count": pkg.spine.len(),
        // Plural forms carry EVERY value. The singular fields above keep the
        // first, so existing consumers are unaffected. (#38)
        "creators": pkg.creators,
        "identifiers": pkg.identifiers,
        "publishers": pkg.publishers,
        "contributors": pkg.contributors,
        "subjects": pkg.subjects,
        "languages": pkg.languages,
        // The book's own chapter list, so a consumer gets titles even when the
        // XHTML uses no heading tags. (#39)
        "toc": pkg.toc.iter().map(|(t, h)| json!({ "title": t, "href": h })).collect::<Vec<_>>(),
    })
}

/// Chunk the whole book: each spine XHTML through the HTML chunker, merged in
/// reading order, with book-level and per-chapter provenance metadata injected.
pub fn chunk_package(
    pkg: &EpubPackage,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<ChunkRecordInput>, String> {
    let meta = doc_metadata(pkg);
    let mut out: Vec<ChunkRecordInput> = Vec::new();

    for (spine_index, doc) in pkg.spine.iter().enumerate() {
        let mut records = build_for_mode(
            &doc.bytes,
            mode,
            window_size,
            overlap,
            sentences_per_chunk,
            paragraphs_per_page,
        )
        // Genuine parse failures now propagate (TECH_DEBT L14). This used to be
        // `.unwrap_or_default()`, which turned ANY chapter error into "this
        // chapter produced no chunks" and so hid real breakage — it is also why
        // bad *arguments* once yielded an empty book instead of an error.
        //
        // The swallow existed because an image-only cover page legitimately has
        // no text, and the HTML builder signalled that with an `Err`. T6 removed
        // that conflation: "parsed fine, nothing to chunk" is now `Ok(vec![])`
        // everywhere, so an `Err` here means the chapter genuinely failed to
        // parse and the caller should hear about it rather than silently getting
        // a shorter book.
        .map_err(|e| format!("spine item {spine_index} ({}): {e}", doc.href))?;

        for rec in &mut records {
            if let serde_json::Value::Object(map) = &mut rec.metadata {
                map.insert("document_metadata".to_string(), meta.clone());
                map.insert("spine_index".to_string(), json!(spine_index));
                map.insert("href".to_string(), json!(doc.href));
                // A TOC's link list reads exactly like prose once chunked.
                // Flag it so retrieval can skip it. (#40)
                map.insert("is_navigation".to_string(), json!(doc.is_navigation));
            }
        }
        out.append(&mut records);
    }
    Ok(out)
}

/// Convert the whole book to a single markdown string (reading order).
pub fn package_to_markdown(pkg: &EpubPackage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(title) = &pkg.title {
        parts.push(format!("# {title}"));
    }
    for doc in &pkg.spine {
        let html = String::from_utf8_lossy(&doc.bytes);
        let md = html_to_markdown_str(&html);
        if !md.trim().is_empty() {
            parts.push(md.trim().to_string());
        }
    }
    parts.join("\n\n---\n\n").trim().to_string()
}
