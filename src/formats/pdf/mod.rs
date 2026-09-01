//! PDF chunking: PDF → markdown → shared markdown pipeline.
//!
//! **The parser is this crate's own** (`parse`), pure Rust and wasm-clean, so
//! `rs-chunks`, `py-chunks` and `js-chunks` all read a PDF with the same code
//! instead of each SDK delegating to its own build of a host parser
//! ([#57](TECH_DEBT.md), [#74](TECH_DEBT.md)). Passing markdown produced
//! elsewhere is still supported through [`chunk_pdf_markdown`], for callers who
//! have their own parser.
//!
//! The one thing still delegated is *rendering*: a text-less scanned PDF returns
//! one raster per page, which needs a graphics engine rather than a parser. That
//! stays behind the native-only `pdf-native` feature.

pub mod author_block;
pub(crate) mod base14;
pub(crate) mod blocks;
pub(crate) mod cambria;
pub(crate) mod cmap;
pub(crate) mod content;
pub(crate) mod doc;
pub(crate) mod encoding_tables;
pub(crate) mod filters;
pub(crate) mod font;
pub(crate) mod geom;
pub(crate) mod glyph_names;
pub(crate) mod images;
pub(crate) mod lines;
pub(crate) mod markdown;
pub(crate) mod page_render;
pub(crate) mod parse;
#[cfg(feature = "pdf-native")]
pub(crate) mod pdfium_render;
pub(crate) mod regions;
pub mod stream;
pub(crate) mod type1;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;

/// Single funnel for every entry style, so they cannot drift: whatever
/// normalisation PDF markdown needs happens here once.
fn pdf_loaded(
    markdown: String,
    images: crate::chunk::ExtractedImages,
    total_pages: usize,
) -> Loaded {
    Loaded {
        markdown: author_block::normalize(&markdown),
        images,
        metadata: serde_json::json!({ "source_type": "pdf", "total_pages": total_pages }),
        records: None,
    }
}

/// Chunk PDF markdown produced by some other parser. `mode` is the usual
/// markdown mode; `total_pages` populates `document_metadata.total_pages`.
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
    pipeline::chunk(
        &loaded,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

/// Like [`chunk_pdf_markdown`] but with caller-supplied images.
// Public API: the signature is mirrored by py-chunks and js-chunks, so it is
// fixed by the cross-SDK contract and cannot be regrouped unilaterally.
#[allow(clippy::too_many_arguments)]
pub fn chunk_pdf_markdown_with_images(
    markdown: &str,
    images: crate::chunk::ExtractedImages,
    total_pages: usize,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    let loaded = pdf_loaded(markdown.to_string(), images, total_pages);
    pipeline::chunk_with_images(
        &loaded,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// `default` is documented as a fast path with minimal font analysis, and
/// `structural` as the full font-size-weighted one. This is where that
/// distinction lives: everything else about the two modes is identical.
pub(crate) fn headings_for(mode: &str) -> parse::Headings {
    if mode == "default" {
        parse::Headings::PerPage
    } else {
        parse::Headings::Ranked
    }
}

fn load(bytes: &[u8], want_images: bool, headings: parse::Headings) -> Result<Loaded> {
    let parsed = parse::parse(bytes, want_images, headings).map_err(ChunkError::Parse)?;
    let mut images = parsed.images;
    // A document with pictures and no prose renders as nothing but `![](…)`
    // references. That is not text, and reporting it as such would hide a
    // scanned PDF behind a page of image links.
    let markdown = if parsed.has_text {
        parsed.markdown
    } else {
        String::new()
    };

    // Say what actually happened. A scanned or otherwise text-less PDF used to
    // fall through to the Markdown chunker and surface as "Markdown file is
    // empty after decoding", which names the wrong format and gives the caller
    // nothing to act on (#56). With list_images on, such a PDF yields one
    // rendered page per page instead, so that path is not an error.
    if markdown.trim().is_empty() && images.is_empty() {
        if want_images && parsed.total_pages > 0 {
            images = page_render::render_pages(bytes)?;
        }
        if images.is_empty() {
            // Name the actual cause. "scanned or image-only" was returned for
            // *any* `has_text == false`, so an encrypted PDF — the commonest
            // real-world failure — got a wrong diagnosis and a remedy that
            // cannot work, since rendering its pages fails for the same reason.
            // The evidence to tell these apart was already being collected and
            // then discarded (TECH_DEBT F8).
            return Err(diagnose(
                parsed.total_pages,
                parsed.encrypted,
                &parsed.skipped,
            ));
        }
    }
    Ok(pdf_loaded(markdown, images, parsed.total_pages))
}

/// First few reasons, so a 500-image document does not produce a 500-line error.
fn preview(reasons: &[String]) -> String {
    const SHOWN: usize = 3;
    let head = reasons
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if reasons.len() > SHOWN {
        format!("{head}; and {} more", reasons.len() - SHOWN)
    } else {
        head
    }
}

/// Say what actually stopped us, given a document with no extractable text.
///
/// Shared so the batch and streaming entry points cannot drift: F8 landed this
/// on `load()` only, and `stream.rs` kept its own copy — so the same encrypted
/// file was reported as encrypted in six modes and as a scan in `default`.
/// One document, two answers, which is the shape F6 fixed for markdown.
pub(super) fn diagnose(total_pages: usize, encrypted: bool, skipped: &[String]) -> ChunkError {
    ChunkError::Parse(if encrypted {
        format!(
            "PDF is encrypted ({total_pages} page(s)); its text and images cannot be read without the password. This is not a scanned document — passing list_images will not help."
        )
    } else if !skipped.is_empty() {
        format!(
            "PDF contains no extractable text ({} page(s)), and {} image(s) could not be decoded: {}. This is not a plain scan — the page content is present but in a form this build cannot read.",
            total_pages,
            skipped.len(),
            preview(skipped)
        )
    } else {
        format!(
            "PDF contains no extractable text ({total_pages} page(s) scanned or image-only). OCR is not enabled; pass list_images to get one rendered image per page."
        )
    })
}

fn read(file_path: &str) -> Result<Vec<u8>> {
    if !file_path.to_ascii_lowercase().ends_with(".pdf") {
        return Err(ChunkError::InvalidArg(format!(
            "Expected .pdf file path, got: {file_path}"
        )));
    }
    std::fs::read(file_path).map_err(|e| ChunkError::Parse(format!("Failed to read PDF: {e}")))
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    chunk_from_bytes(
        &read(file_path)?,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

pub fn chunk_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    pipeline::chunk(
        &load(bytes, false, headings_for(mode))?,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    {
        let mode = crate::formats::pipeline::mode_str(opts.mode)?;
        pipeline::chunk_opts(&load(&read(file_path)?, false, headings_for(mode))?, opts)
    }
}

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    chunk_with_images_from_bytes(
        &read(file_path)?,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

pub fn chunk_with_images_from_bytes(
    bytes: &[u8],
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<crate::chunk::ChunksWithImages> {
    pipeline::chunk_with_images(
        &load(bytes, true, headings_for(mode))?,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    to_markdown_from_bytes(&read(file_path)?)
}

pub fn to_markdown_from_bytes(bytes: &[u8]) -> Result<String> {
    Ok(load(bytes, false, parse::Headings::Ranked)?.markdown)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<crate::chunk::MarkdownWithImages> {
    to_markdown_with_images_from_bytes(&read(file_path)?)
}

pub fn to_markdown_with_images_from_bytes(
    bytes: &[u8],
) -> Result<crate::chunk::MarkdownWithImages> {
    let loaded = load(bytes, true, parse::Headings::Ranked)?;
    Ok((loaded.markdown, loaded.images))
}

/// Stream a PDF's chunks. Reading the file happens here — so a missing or
/// misnamed path still fails at construction — but parsing and chunking do not
/// (see [`stream`](mod@stream) for what streaming can and cannot do for PDF).
pub fn stream(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<stream::PdfChunkStream> {
    Ok(stream_from_bytes(
        read(file_path)?,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    ))
}

/// Stream a PDF's chunks from bytes.
pub fn stream_from_bytes(
    bytes: Vec<u8>,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> stream::PdfChunkStream {
    stream::stream_from_bytes(
        bytes,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )
}
