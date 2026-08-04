//! Spreadsheet chunking (`.xlsx` / `.xls` / `.xlsm` / `.xlsb` / `.ods` / `.xltx`
//! / `.xltm`) via calamine. Modes: row, table, sheet, semantic, page_aware,
//! sliding_window.

pub mod common;
pub mod images;
pub mod repair;
mod page_aware;
mod semantic;
mod sheet;
mod sliding_window;
mod table_region;
mod to_markdown;

use calamine::Reader;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use common::{build_row_chunks, XlsxChunkRecord};

fn to_chunks(records: Vec<XlsxChunkRecord>) -> Vec<Chunk> {
    records
        .into_iter()
        .map(|c| Chunk::new(c.content, c.content_type, c.metadata))
        .collect()
}

fn ensure_spreadsheet(file_path: &str) -> Result<()> {
    if common::is_supported_spreadsheet(file_path) {
        Ok(())
    } else {
        Err(ChunkError::InvalidArg(format!(
            "Expected a spreadsheet file ({}), got: {file_path}",
            common::supported_spreadsheet_exts_display()
        )))
    }
}

/// Faithful mirror of the reference `chunk_xlsx`.
#[allow(clippy::too_many_arguments)]
pub fn chunk(
    file_path: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
    max_chunk_chars: usize,
) -> Result<Vec<Chunk>> {
    ensure_spreadsheet(file_path)?;
    let data = std::fs::read(file_path).map_err(ChunkError::Io)?;
    let ext = ext_of(file_path);
    chunk_from_bytes(&data, &ext, mode, rows_per_chunk, window_size, overlap, include_headers, sheet_names, skip_empty_rows, max_chunk_chars)
}

fn ext_of(file_path: &str) -> String {
    std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// No-filesystem entry (wasm/browser). `ext` (e.g. "xlsx","ods") routes format
/// detection for calamine + the ODS mimetype repair.
#[allow(clippy::too_many_arguments)]
pub fn chunk_from_bytes(
    data: &[u8],
    ext: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
    max_chunk_chars: usize,
) -> Result<Vec<Chunk>> {
    if rows_per_chunk < 1 {
        return Err(ChunkError::InvalidArg("rows_per_chunk must be greater than 0".into()));
    }
    if max_chunk_chars < 1 {
        return Err(ChunkError::InvalidArg("max_chunk_chars must be greater than 0".into()));
    }
    let res = match mode {
        "row" | "default" => build_row_chunks(data, ext, rows_per_chunk, include_headers, sheet_names, skip_empty_rows),
        "table" => table_region::build_table_chunks(data, ext, include_headers, sheet_names, skip_empty_rows, max_chunk_chars),
        "sheet" => sheet::build_sheet_chunks(data, ext, include_headers, sheet_names, skip_empty_rows, max_chunk_chars),
        "semantic" => semantic::build_semantic_chunks(data, ext, rows_per_chunk, include_headers, sheet_names, skip_empty_rows),
        "page_aware" => page_aware::build_page_aware_chunks(data, ext, include_headers, sheet_names, skip_empty_rows, max_chunk_chars),
        "sliding_window" => {
            if window_size < 1 {
                return Err(ChunkError::InvalidArg("window_size must be >= 1".into()));
            }
            if overlap >= window_size {
                return Err(ChunkError::InvalidArg("overlap must be less than window_size".into()));
            }
            sliding_window::build_sliding_window_chunks(data, ext, window_size, overlap, include_headers, sheet_names, skip_empty_rows)
        }
        other => return Err(ChunkError::InvalidArg(format!(
            "mode must be one of [page_aware, row, semantic, sheet, sliding_window, table] for XLSX, got: '{other}'"
        ))),
    };
    res.map(to_chunks).map_err(ChunkError::Parse)
}

/// Dispatch entry mirroring Python `get_chunks` spreadsheet routing.
pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    let mode = match opts.mode {
        ChunkMode::Default | ChunkMode::Row => "row",
        ChunkMode::Table => "table",
        ChunkMode::Sheet => "sheet",
        ChunkMode::Semantic => "semantic",
        ChunkMode::PageAware => "page_aware",
        ChunkMode::SlidingWindow => "sliding_window",
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "XLSX does not support mode '{}'",
                other.as_str()
            )))
        }
    };
    let rows_per_chunk = if opts.sentences_per_chunk == 3 { 1 } else { opts.sentences_per_chunk };
    chunk(
        file_path,
        mode,
        rows_per_chunk,
        opts.window_size,
        opts.overlap,
        true,
        Vec::new(),
        true,
        2000,
    )
}

/// Chunk with embedded images: image chunks (with sheet provenance) first, then
/// the text chunks, plus the extracted image bytes. `.xls` yields no images.
#[allow(clippy::too_many_arguments)]
pub fn chunk_with_images(
    file_path: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
    max_chunk_chars: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    ensure_spreadsheet(file_path)?;
    let data = std::fs::read(file_path).map_err(ChunkError::Io)?;
    let ext = ext_of(file_path);
    chunk_with_images_from_bytes(&data, &ext, mode, rows_per_chunk, window_size, overlap, include_headers, sheet_names, skip_empty_rows, max_chunk_chars)
}

/// No-filesystem `chunk_with_images` (wasm/browser).
#[allow(clippy::too_many_arguments)]
pub fn chunk_with_images_from_bytes(
    data: &[u8],
    ext: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
    max_chunk_chars: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    if rows_per_chunk < 1 {
        return Err(ChunkError::InvalidArg("rows_per_chunk must be greater than 0".into()));
    }
    if max_chunk_chars < 1 {
        return Err(ChunkError::InvalidArg("max_chunk_chars must be greater than 0".into()));
    }
    let workbook = common::open_spreadsheet_from_bytes(data, ext).map_err(ChunkError::Parse)?;
    let all_sheet_names = workbook.sheet_names().to_vec();
    drop(workbook);

    let normalized = if mode == "default" { "row" } else { mode };
    let text_records = match normalized {
        "row" => build_row_chunks(data, ext, rows_per_chunk, include_headers, sheet_names, skip_empty_rows),
        "table" => table_region::build_table_chunks(data, ext, include_headers, sheet_names, skip_empty_rows, max_chunk_chars),
        "sheet" => sheet::build_sheet_chunks(data, ext, include_headers, sheet_names, skip_empty_rows, max_chunk_chars),
        "semantic" => semantic::build_semantic_chunks(data, ext, rows_per_chunk, include_headers, sheet_names, skip_empty_rows),
        "page_aware" => page_aware::build_page_aware_chunks(data, ext, include_headers, sheet_names, skip_empty_rows, max_chunk_chars),
        "sliding_window" => {
            if window_size < 1 {
                return Err(ChunkError::InvalidArg("window_size must be >= 1".into()));
            }
            if overlap >= window_size {
                return Err(ChunkError::InvalidArg("overlap must be less than window_size".into()));
            }
            sliding_window::build_sliding_window_chunks(data, ext, window_size, overlap, include_headers, sheet_names, skip_empty_rows)
        }
        other => return Err(ChunkError::InvalidArg(format!("Unknown XLSX mode: {other}"))),
    }
    .map_err(ChunkError::Parse)?;

    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    let image_infos = images::collect_spreadsheet_images(data, ext, &all_sheet_names, &mut image_out);
    let mut chunks: Vec<Chunk> = image_infos
        .into_iter()
        .map(|info| {
            Chunk::new(
                info.hash_name.clone(),
                "image",
                serde_json::json!({
                    "sheet_name": info.sheet_name,
                    "sheet_index": info.sheet_index,
                    "image_name": info.hash_name,
                    "alt_text": info.alt_text,
                }),
            )
        })
        .collect();
    chunks.extend(text_records.into_iter().map(|r| Chunk::new(r.content, r.content_type, r.metadata)));
    Ok((chunks, image_out))
}

pub fn to_markdown(file_path: &str) -> Result<String> {
    ensure_spreadsheet(file_path)?;
    let data = std::fs::read(file_path).map_err(ChunkError::Io)?;
    to_markdown::to_markdown(&data, &ext_of(file_path)).map_err(ChunkError::Parse)
}

pub fn to_markdown_from_bytes(data: &[u8], ext: &str) -> Result<String> {
    to_markdown::to_markdown(data, ext).map_err(ChunkError::Parse)
}

pub fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    ensure_spreadsheet(file_path)?;
    let data = std::fs::read(file_path).map_err(ChunkError::Io)?;
    to_markdown::to_markdown_with_images(&data, &ext_of(file_path)).map_err(ChunkError::Parse)
}

pub fn to_markdown_with_images_from_bytes(data: &[u8], ext: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    to_markdown::to_markdown_with_images(data, ext).map_err(ChunkError::Parse)
}

pub fn stream(
    file_path: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
) -> Result<impl Iterator<Item = Result<Chunk>>> {
    Ok(chunk(file_path, mode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000)?
        .into_iter()
        .map(Ok))
}
