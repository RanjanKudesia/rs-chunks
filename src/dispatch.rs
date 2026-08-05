//! Source-agnostic dispatch: route a file to the right engine by extension,
//! mirroring the Python `get_chunks()` routing (including the delimited/
//! spreadsheet special-casing) so behaviour matches the reference package.

use std::path::Path;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats;

/// Chunk a document supplied as raw bytes. `filename` is used only for extension
/// detection (routing) — never persisted under that name.
///
/// Routes to each format's no-filesystem `chunk_from_bytes` where available (these
/// work on wasm32). Formats not yet refactored fall back to a temp file on native
/// targets; on wasm32 they return [`ChunkError::Unsupported`] until refactored.
pub fn get_chunks_from_bytes(
    data: &[u8],
    filename: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "csv" | "tsv" => {
            let csv_mode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if csv_mode == "page_aware" { paragraphs_per_page } else { csv_rows_per_chunk(sentences_per_chunk) };
            let delimiter = if ext == "tsv" { Some(b'\t') } else { None };
            formats::csv::chunk_from_bytes(data, csv_mode, rows_per_chunk, window_size, overlap, true, delimiter, "utf-8", true)
        }
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => {
            let xmode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if sentences_per_chunk == 3 { 1 } else { sentences_per_chunk };
            formats::xlsx::chunk_from_bytes(data, &ext, xmode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000)
        }
        "md" => formats::md::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "txt" => formats::txt::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "html" | "htm" => formats::html::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "json" | "jsonl" | "ndjson" => formats::json::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "eml" | "mbox" => formats::eml::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "odt" | "odp" => formats::odf::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ipynb" => formats::ipynb::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "rtf" => formats::rtf::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "epub" => formats::epub::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "msg" => formats::msg::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "doc" => formats::doc::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ppt" => formats::ppt::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pdf" => formats::pdf::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        _ => bytes_fallback_chunks(data, &ext, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
    }
}

/// Convert bytes to Markdown; see [`get_chunks_from_bytes`] for the routing note.
pub fn get_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "csv" => formats::csv::to_markdown_from_bytes(data, None, "utf-8"),
        "tsv" => formats::csv::to_markdown_from_bytes(data, Some(b'\t'), "utf-8"),
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => formats::xlsx::to_markdown_from_bytes(data, &ext),
        "md" => formats::md::to_markdown_from_bytes(data),
        "txt" => formats::txt::to_markdown_from_bytes(data),
        "html" | "htm" => formats::html::to_markdown_from_bytes(data),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown_from_bytes(data),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown_from_bytes(data),
        "json" | "jsonl" | "ndjson" => formats::json::to_markdown_from_bytes(data, filename),
        "eml" | "mbox" => formats::eml::to_markdown_from_bytes(data, filename),
        "odt" | "odp" => formats::odf::to_markdown_from_bytes(data, filename),
        "ipynb" => formats::ipynb::to_markdown_from_bytes(data),
        "rtf" => formats::rtf::to_markdown_from_bytes(data),
        "epub" => formats::epub::to_markdown_from_bytes(data),
        "msg" => formats::msg::to_markdown_from_bytes(data),
        "doc" => formats::doc::to_markdown_from_bytes(data),
        "ppt" => formats::ppt::to_markdown_from_bytes(data),
        "pdf" => formats::pdf::to_markdown_from_bytes(data),
        _ => bytes_fallback_markdown(data, &ext),
    }
}

/// Chunk bytes and also return extracted image bytes (`list_images=True`).
/// Formats without embedded-image support return an empty image list and the
/// same chunks as [`get_chunks_from_bytes`].
#[allow(clippy::too_many_arguments)]
pub fn get_chunks_with_images_from_bytes(
    data: &[u8],
    filename: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => {
            let xmode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if sentences_per_chunk == 3 { 1 } else { sentences_per_chunk };
            formats::xlsx::chunk_with_images_from_bytes(data, &ext, xmode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000)
        }
        "html" | "htm" => formats::html::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "eml" | "mbox" => formats::eml::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "msg" => formats::msg::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "odt" | "odp" => formats::odf::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ipynb" => formats::ipynb::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "epub" => formats::epub::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "doc" => formats::doc::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ppt" => formats::ppt::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pdf" => formats::pdf::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        // No embedded-image support: chunks only, empty image list.
        _ => Ok((get_chunks_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?, Vec::new())),
    }
}

/// Convert bytes to Markdown and return extracted image bytes (`list_images=True`).
pub fn get_markdown_with_images_from_bytes(data: &[u8], filename: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => formats::xlsx::to_markdown_with_images_from_bytes(data, &ext),
        "html" | "htm" => formats::html::to_markdown_with_images_from_bytes(data),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown_with_images_from_bytes(data),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown_with_images_from_bytes(data),
        "eml" | "mbox" => formats::eml::to_markdown_with_images_from_bytes(data, filename),
        "msg" => formats::msg::to_markdown_with_images_from_bytes(data),
        "odt" | "odp" => formats::odf::to_markdown_with_images_from_bytes(data, filename),
        "ipynb" => formats::ipynb::to_markdown_with_images_from_bytes(data),
        "epub" => formats::epub::to_markdown_with_images_from_bytes(data),
        "doc" => formats::doc::to_markdown_with_images_from_bytes(data),
        "ppt" => formats::ppt::to_markdown_with_images_from_bytes(data),
        "pdf" => formats::pdf::to_markdown_with_images_from_bytes(data),
        _ => Ok((get_markdown_from_bytes(data, filename)?, Vec::new())),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn bytes_fallback_chunks(
    data: &[u8],
    ext: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let tmp = TempFile::new(data, ext)?;
    get_chunks(tmp.path(), mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn bytes_fallback_chunks(_: &[u8], ext: &str, _: &str, _: usize, _: usize, _: usize, _: usize) -> Result<Vec<Chunk>> {
    Err(ChunkError::Unsupported(format!("bytes chunking for '.{ext}' is not yet available on wasm32")))
}

#[cfg(not(target_arch = "wasm32"))]
fn bytes_fallback_markdown(data: &[u8], ext: &str) -> Result<String> {
    let tmp = TempFile::new(data, ext)?;
    get_markdown(tmp.path())
}

#[cfg(target_arch = "wasm32")]
fn bytes_fallback_markdown(_: &[u8], ext: &str) -> Result<String> {
    Err(ChunkError::Unsupported(format!("bytes markdown for '.{ext}' is not yet available on wasm32")))
}

/// Minimal RAII temp file (no external deps). Removed on drop.
#[cfg(not(target_arch = "wasm32"))]
struct TempFile {
    path: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl TempFile {
    fn new(data: &[u8], ext: &str) -> Result<Self> {
        use std::io::Write;
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "chunks_rs_{}_{}.{ext}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        dir.push(unique);
        let path = dir.to_string_lossy().to_string();
        let mut f = std::fs::File::create(&path).map_err(ChunkError::Io)?;
        f.write_all(data).map_err(ChunkError::Io)?;
        Ok(TempFile { path })
    }
    fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn ext_of(file_path: &str) -> String {
    Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn csv_rows_per_chunk(sentences_per_chunk: usize) -> usize {
    sentences_per_chunk.max(1)
}

/// Chunk any supported document by path. `mode` is passed through to the engine
/// ("default" selects each format's natural strategy); the delimited formats map
/// it onto their row/window/page strategies exactly like the Python entry point.
pub fn get_chunks(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let ext = ext_of(file_path);
    match ext.as_str() {
        // ── Delimited text ──────────────────────────────────────────────
        "csv" | "tsv" => {
            let csv_mode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if csv_mode == "page_aware" {
                paragraphs_per_page
            } else {
                csv_rows_per_chunk(sentences_per_chunk)
            };
            let delimiter = if ext == "tsv" { Some(b'\t') } else { None };
            formats::csv::chunk(
                file_path,
                csv_mode,
                rows_per_chunk,
                window_size,
                overlap,
                true,
                delimiter,
                "utf-8",
                true,
            )
        }
        // ── Spreadsheets (calamine) ─────────────────────────────────────
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => {
            let xmode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if sentences_per_chunk == 3 { 1 } else { sentences_per_chunk };
            formats::xlsx::chunk(
                file_path, xmode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000,
            )
        }
        "doc" => formats::doc::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        // ── Word OOXML ──────────────────────────────────────────────────
        "docx" | "docm" | "dotx" | "dotm" => {
            formats::docx::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
        }
        // ── Prose / markdown-pipeline formats ───────────────────────────
        "ppt" => formats::ppt::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => {
            formats::pptx::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
        }
        "md" => formats::md::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "txt" => formats::txt::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "html" | "htm" => formats::html::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "json" | "jsonl" | "ndjson" => formats::json::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "eml" | "mbox" => formats::eml::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "odt" | "odp" => formats::odf::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "msg" => formats::msg::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ipynb" => formats::ipynb::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "rtf" => formats::rtf::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pdf" => formats::pdf::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "epub" => formats::epub::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        other => Err(ChunkError::Unsupported(format!("Unsupported file type '.{other}'"))),
    }
}

/// Convert a supported document to Markdown by path.
pub fn get_markdown(file_path: &str) -> Result<String> {
    let ext = ext_of(file_path);
    match ext.as_str() {
        "csv" | "tsv" => {
            let delimiter = if ext == "tsv" { Some(b'\t') } else { None };
            formats::csv::to_markdown(file_path, delimiter, "utf-8")
        }
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => formats::xlsx::to_markdown(file_path),
        "doc" => formats::doc::to_markdown(file_path),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown(file_path),
        "ppt" => formats::ppt::to_markdown(file_path),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown(file_path),
        "md" => formats::md::to_markdown(file_path),
        "txt" => formats::txt::to_markdown(file_path),
        "html" | "htm" => formats::html::to_markdown(file_path),
        "json" | "jsonl" | "ndjson" => formats::json::to_markdown(file_path),
        "eml" | "mbox" => formats::eml::to_markdown(file_path),
        "odt" | "odp" => formats::odf::to_markdown(file_path),
        "msg" => formats::msg::to_markdown(file_path),
        "ipynb" => formats::ipynb::to_markdown(file_path),
        "rtf" => formats::rtf::to_markdown(file_path),
        "pdf" => formats::pdf::to_markdown(file_path),
        "epub" => formats::epub::to_markdown(file_path),
        other => Err(ChunkError::Unsupported(format!("get_markdown does not support '.{other}'"))),
    }
}
