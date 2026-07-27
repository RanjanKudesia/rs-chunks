//! EPUB chunking: walk the spine (reading order) and reuse the HTML chunker on
//! each content document.

pub mod extract;
pub mod package;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use extract::{chunk_package, package_to_markdown};
use package::{parse, EpubPackage};

fn ensure_epub(file_path: &str) -> Result<()> {
    if file_path.to_ascii_lowercase().ends_with(".epub") {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected .epub file path, got: {file_path}"
        )))
    }
}

fn image_key(href: &str) -> String {
    href.rsplit('/').next().unwrap_or(href).to_string()
}

fn load(file_path: &str) -> Result<EpubPackage> {
    ensure_epub(file_path)?;
    let bytes = std::fs::read(file_path).map_err(ChunkError::Io)?;
    parse(bytes).map_err(ChunkError::Parse)
}

pub fn chunk(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let pkg = load(file_path)?;
    chunk_pkg(&pkg, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

fn chunk_pkg(pkg: &EpubPackage, mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    let records = chunk_package(pkg, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
        .map_err(ChunkError::Parse)?;
    Ok(records
        .into_iter()
        .map(|r| Chunk::new(r.content, r.content_type.as_str(), r.metadata))
        .collect())
}

/// No-filesystem entry (wasm/browser).
pub fn chunk_from_bytes(data: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<Vec<Chunk>> {
    let pkg = parse(data.to_vec()).map_err(ChunkError::Parse)?;
    chunk_pkg(&pkg, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

pub fn to_markdown_from_bytes(data: &[u8]) -> Result<String> {
    let pkg = parse(data.to_vec()).map_err(ChunkError::Parse)?;
    Ok(package_to_markdown(&pkg))
}

pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    let mode = match opts.mode {
        ChunkMode::Default => "default",
        ChunkMode::Structural => "structural",
        ChunkMode::Section => "section",
        ChunkMode::Semantic => "semantic",
        ChunkMode::Sentence => "sentence",
        ChunkMode::PageAware => "page_aware",
        ChunkMode::SlidingWindow => "sliding_window",
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "EPUB does not support mode '{}'",
                other.as_str()
            )))
        }
    };
    chunk(
        file_path,
        mode,
        opts.window_size,
        opts.overlap,
        opts.sentences_per_chunk,
        opts.paragraphs_per_page,
    )
}

pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let pkg = load(file_path)?;
    let records = chunk_package(&pkg, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
        .map_err(ChunkError::Parse)?;

    let mut chunks: Vec<Chunk> = pkg
        .images
        .iter()
        .map(|(href, _)| {
            let key = image_key(href);
            Chunk::new(
                key.clone(),
                "image",
                serde_json::json!({ "image_name": key, "href": href }),
            )
        })
        .collect();
    chunks.extend(
        records
            .into_iter()
            .map(|r| Chunk::new(r.content, r.content_type.as_str(), r.metadata)),
    );
    let images = pkg.images.iter().map(|(href, b)| (image_key(href), b.clone())).collect();
    Ok((chunks, images))
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    Ok(package_to_markdown(&load(file_path)?))
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let pkg = load(file_path)?;
    let md = package_to_markdown(&pkg);
    let images = pkg.images.iter().map(|(href, b)| (image_key(href), b.clone())).collect();
    Ok((md, images))
}


/// No-filesystem `chunk_with_images` (wasm/browser).
pub fn chunk_with_images_from_bytes(data: &[u8], mode: &str, window_size: usize, overlap: usize, sentences_per_chunk: usize, paragraphs_per_page: usize) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let pkg = parse(data.to_vec()).map_err(ChunkError::Parse)?;
    let records = chunk_package(&pkg, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page).map_err(ChunkError::Parse)?;
    let mut chunks: Vec<Chunk> = pkg.images.iter().map(|(href, _)| { let key = image_key(href); Chunk::new(key.clone(), "image", serde_json::json!({"image_name": key, "href": href})) }).collect();
    chunks.extend(records.into_iter().map(|r| Chunk::new(r.content, r.content_type.as_str(), r.metadata)));
    let images = pkg.images.iter().map(|(href, b)| (image_key(href), b.clone())).collect();
    Ok((chunks, images))
}

pub fn to_markdown_with_images_from_bytes(data: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let pkg = parse(data.to_vec()).map_err(ChunkError::Parse)?;
    let md = package_to_markdown(&pkg);
    let images = pkg.images.iter().map(|(href, b)| (image_key(href), b.clone())).collect();
    Ok((md, images))
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
