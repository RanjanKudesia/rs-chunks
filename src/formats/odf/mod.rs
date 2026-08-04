//! `.odt` / `.odp` OpenDocument chunking (markdown pipeline, with images).

pub mod container;
pub mod text;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use container::{load as load_container, parse_meta, OdfKind};
use text::content_to_markdown;

fn kind_for(file_path: &str) -> Result<OdfKind> {
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".odt") {
        Ok(OdfKind::Text)
    } else if lower.ends_with(".odp") {
        Ok(OdfKind::Presentation)
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .odt or .odp file path, got: {file_path}"
        )))
    }
}

fn load(file_path: &str) -> Result<Loaded> {
    let bytes = std::fs::read(file_path).map_err(ChunkError::Io)?;
    load_bytes(&bytes, file_path)
}

/// No-filesystem entry (wasm/browser). `filename` routes `.odt` vs `.odp`.
pub fn chunk_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    chunk_odp(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    Ok(load_bytes(data, filename)?.markdown)
}

/// `pipeline::chunk` plus the slide identity `.odp` needs. `.odt` has no
/// slides, so the pass is a no-op there.
fn chunk_odp(
    loaded: &Loaded,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let mut chunks = pipeline::chunk(
        loaded,
        mode,
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    )?;
    if loaded
        .metadata
        .get("source_type")
        .and_then(|v| v.as_str())
        == Some("odp")
    {
        inject_slide_metadata(&mut chunks);
    }
    Ok(chunks)
}

/// Give `.odp` chunks the slide identity `.pptx` chunks already carry.
///
/// Slide identity existed only as an unstructured `## Slide N` markdown
/// heading, so a consumer had to string-parse `section_heading` to answer
/// "which slide is this?" — while the same question on a `.pptx` is a metadata
/// lookup. (#52)
///
/// The title is the slide's first line of text, which is what a reader would
/// call its title; `.odp` has no title element the way `.pptx` does.
fn inject_slide_metadata(chunks: &mut [Chunk]) {
    let mut slide: Option<u64> = None;
    let mut title: Option<String> = None;
    let mut titles: std::collections::HashMap<u64, String> = std::collections::HashMap::new();

    for chunk in chunks.iter_mut() {
        let heading_here = slide_number_of(chunk.content.trim());
        let heading_ctx = chunk
            .metadata
            .get("section_heading")
            .and_then(|v| v.as_str())
            .and_then(slide_number_of);

        if let Some(n) = heading_here {
            // The `## Slide N` heading chunk itself starts a new slide.
            slide = Some(n);
            title = None;
        } else if let Some(n) = heading_ctx {
            if slide != Some(n) {
                slide = Some(n);
                title = None;
            }
            if title.is_none() {
                title = chunk
                    .content
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(str::to_string);
            }
        }

        if let Some(n) = slide {
            if let Some(t) = &title {
                titles.entry(n).or_insert_with(|| t.clone());
            }
            if let Some(map) = chunk.metadata.as_object_mut() {
                map.insert("slide_number".into(), serde_json::json!(n));
            }
        }
    }

    // Second pass: the `## Slide N` heading chunk is emitted before the slide's
    // first line of text, so its title is not known yet on the way through.
    // Backfill it, rather than leave one chunk per slide with a null title
    // while its siblings have one.
    for chunk in chunks.iter_mut() {
        let Some(n) = chunk
            .metadata
            .get("slide_number")
            .and_then(|v| v.as_u64())
        else {
            continue;
        };
        let t = titles.get(&n).cloned();
        if let Some(map) = chunk.metadata.as_object_mut() {
            map.insert("slide_title".into(), serde_json::json!(t));
        }
    }
}

/// `"Slide 4"` -> `Some(4)`.
fn slide_number_of(text: &str) -> Option<u64> {
    text.strip_prefix("Slide ")?.trim().parse().ok()
}

fn load_bytes(bytes: &[u8], filename: &str) -> Result<Loaded> {
    let kind = kind_for(filename)?;
    let container = load_container(bytes, kind).map_err(ChunkError::Parse)?;
    let (markdown, slide_count) = content_to_markdown(&container.content_xml, kind, &container.image_names);
    let (title, creator) = container
        .meta_xml
        .as_deref()
        .map(parse_meta)
        .unwrap_or((None, None));
    let metadata = match kind {
        OdfKind::Text => serde_json::json!({
            "source_type": "odt", "title": title, "creator": creator,
        }),
        OdfKind::Presentation => serde_json::json!({
            "source_type": "odp", "title": title, "creator": creator, "slide_count": slide_count,
        }),
    };
    Ok(Loaded { markdown, images: container.images, metadata })
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    chunk_odp(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    pipeline::chunk_opts(&load(file_path)?, opts)
}

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(load(file_path)?.markdown)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load(file_path)?;
    Ok((l.markdown, crate::formats::pipeline::dedup_images(l.images)))
}

/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(data: &[u8], filename: &str, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    pipeline::chunk_with_images(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_with_images_from_bytes(data: &[u8], filename: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load_bytes(data, filename)?;
    Ok((l.markdown, crate::formats::pipeline::dedup_images(l.images)))
}


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
