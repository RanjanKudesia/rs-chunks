//! PDF chunking: PDF → markdown → shared markdown pipeline.
//!
//! Two entry styles:
//! - **Native** (`pdf-native` feature, default): parses the PDF with the
//!   `liteparse` crate (PDFium). Path-based [`chunk`], [`to_markdown`], etc.
//! - **Host-composed** (always available, incl. wasm32): the caller supplies the
//!   markdown + page count (e.g. from `@llamaindex/liteparse-wasm`) to
//!   [`chunk_pdf_markdown`] / [`chunk_pdf_markdown_with_images`]. This is how the
//!   browser/WASM build handles PDF without bundling PDFium.

#[cfg(feature = "pdf-native")]
pub mod liteparse_backend;

use crate::chunk::Chunk;
use crate::error::Result;
use crate::formats::pipeline::{self, Loaded};

#[cfg(feature = "pdf-native")]
use crate::error::ChunkError;
#[cfg(feature = "pdf-native")]
use crate::options::ChunkOptions;

fn pdf_loaded(markdown: String, images: Vec<(String, Vec<u8>)>, total_pages: usize) -> Loaded {
    Loaded {
        markdown,
        images,
        metadata: serde_json::json!({ "source_type": "pdf", "total_pages": total_pages }),
    }
}

/// Chunk PDF markdown produced by a host-side parser (no PDFium). `mode` is the
/// usual markdown mode; `total_pages` populates `document_metadata.total_pages`.
pub fn chunk_pdf_markdown(
    markdown: &str,
    total_pages: usize,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let loaded = pdf_loaded(markdown.to_string(), Vec::new(), total_pages);
    pipeline::chunk(&loaded, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

/// Like [`chunk_pdf_markdown`] but with host-supplied images (image chunks first).
pub fn chunk_pdf_markdown_with_images(
    markdown: &str,
    images: Vec<(String, Vec<u8>)>,
    total_pages: usize,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let loaded = pdf_loaded(markdown.to_string(), images, total_pages);
    pipeline::chunk_with_images(&loaded, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

// ── Native (liteparse/PDFium) path ──────────────────────────────────────────

#[cfg(feature = "pdf-native")]
fn ensure_pdf(file_path: &str) -> Result<()> {
    if file_path.to_ascii_lowercase().ends_with(".pdf") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!("Expected .pdf file path, got: {file_path}")))
    }
}

#[cfg(feature = "pdf-native")]
fn load(file_path: &str, embed_images: bool) -> Result<Loaded> {
    ensure_pdf(file_path)?;
    let conv = liteparse_backend::pdf_to_markdown(file_path, embed_images).map_err(ChunkError::Parse)?;
    // Say what actually happened. A scanned or otherwise text-less PDF used to
    // fall through to the Markdown chunker and surface as "Markdown file is
    // empty after decoding", which names the wrong format and gives the caller
    // nothing to act on. (#56)
    // No text AND no images means there is genuinely nothing to return. With
    // list_images on, a scanned PDF yields one rendered image per page instead
    // (supported-formats.mdx), so that path is not an error.
    if conv.markdown.trim().is_empty() && conv.images.is_empty() {
        return Err(ChunkError::Parse(format!(
            "PDF contains no extractable text ({} page(s) scanned or image-only). OCR is not enabled; pass list_images to get one rendered image per page.",
            conv.total_pages
        )));
    }
    let images = conv.images.into_iter().map(|img| (img.name, img.bytes)).collect();
    Ok(pdf_loaded(conv.markdown, images, conv.total_pages))
}

#[cfg(feature = "pdf-native")]
pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    pipeline::chunk(&load(file_path, false)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

#[cfg(feature = "pdf-native")]
pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    pipeline::chunk_opts(&load(file_path, false)?, opts)
}

#[cfg(feature = "pdf-native")]
pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load(file_path, true)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

#[cfg(feature = "pdf-native")]
pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(load(file_path, false)?.markdown)
}

#[cfg(feature = "pdf-native")]
pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load(file_path, true)?;
    Ok((l.markdown, l.images))
}

#[cfg(feature = "pdf-native")]
pub fn stream(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<impl Iterator<Item = Result<Chunk>>> {
    Ok(chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?
        .into_iter()
        .map(Ok))
}
