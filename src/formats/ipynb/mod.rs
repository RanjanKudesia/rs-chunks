//! `.ipynb` Jupyter notebook chunking (markdown pipeline, with images).

pub mod extract;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::pipeline::{self, Loaded};
use crate::options::ChunkOptions;
use extract::{extract, to_markdown as nb_to_markdown};

fn ensure_ipynb(file_path: &str) -> Result<()> {
    if file_path.to_ascii_lowercase().ends_with(".ipynb") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .ipynb file path, got: {file_path}"
        )))
    }
}

fn load(file_path: &str) -> Result<Loaded> {
    ensure_ipynb(file_path)?;
    let bytes = std::fs::read(file_path).map_err(ChunkError::Io)?;
    load_bytes(&bytes)
}

/// No-filesystem entry (wasm/browser).
pub fn chunk_from_bytes(data: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    swallow_empty(pipeline::chunk(&load_bytes(data)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page))
}

pub fn to_markdown_from_bytes(data: &[u8]) -> Result<String> {
    Ok(load_bytes(data)?.markdown)
}

fn load_bytes(bytes: &[u8]) -> Result<Loaded> {
    let doc = extract(bytes).map_err(ChunkError::Parse)?;
    let markdown = nb_to_markdown(&doc);
    let metadata = serde_json::json!({
        "source_type": "ipynb",
        "language": doc.language,
        "kernel": doc.kernel,
        "nbformat": doc.nbformat,
        "cell_count": doc.cell_count,
        "code_cell_count": doc.code_cell_count,
        "markdown_cell_count": doc.markdown_cell_count,
    });
    Ok(Loaded { markdown, images: doc.images, metadata, records: None })
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    swallow_empty(pipeline::chunk(&load(file_path)?, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page))
}

/// A notebook whose only content is images (or is otherwise text-empty) yields
/// no markdown chunks. Match the reference engine: return zero chunks rather
/// than surfacing the "empty"/"No chunks" parse error.
fn swallow_empty(res: Result<Vec<Chunk>>) -> Result<Vec<Chunk>> {
    match res {
        Err(ChunkError::Parse(e)) if e.contains("empty") || e.contains("No chunks") => Ok(Vec::new()),
        other => other,
    }
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    swallow_empty(pipeline::chunk_opts(&load(file_path)?, opts))
}

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let loaded = load(file_path)?;
    let mut chunks: Vec<Chunk> = loaded
        .images
        .iter()
        .map(|(name, _)| Chunk::new(name.clone(), "image", serde_json::json!({ "image_name": name })))
        .collect();
    let text = swallow_empty(pipeline::chunk(
        &loaded, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page,
    ))?;
    chunks.extend(text);
    Ok((chunks, loaded.images.clone()))
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(load(file_path)?.markdown)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load(file_path)?;
    Ok((l.markdown, crate::formats::pipeline::dedup_images(l.images)))
}

/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(data: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let loaded = load_bytes(data)?;
    let mut chunks: Vec<Chunk> = loaded.images.iter().map(|(name, _)| Chunk::new(name.clone(), "image", serde_json::json!({ "image_name": name }))).collect();
    let text = swallow_empty(pipeline::chunk(&loaded, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page))?;
    chunks.extend(text);
    Ok((chunks, loaded.images.clone()))
}

pub fn to_markdown_with_images_from_bytes(data: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let l = load_bytes(data)?;
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
