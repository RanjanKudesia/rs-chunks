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
    pipeline::chunk(&load_bytes(data, filename)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    Ok(load_bytes(data, filename)?.markdown)
}

fn load_bytes(bytes: &[u8], filename: &str) -> Result<Loaded> {
    let kind = kind_for(filename)?;
    let container = load_container(bytes, kind).map_err(ChunkError::Parse)?;
    let (markdown, slide_count) = content_to_markdown(&container.content_xml, kind);
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
    pipeline::chunk(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
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
