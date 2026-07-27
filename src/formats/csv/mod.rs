//! CSV / TSV chunking.
//!
//! Native entry points:
//! - [`chunk`] — faithful, format-specific signature (mirrors the old pyfunction).
//! - [`chunk_with_options`] — the dispatch-layer entry from [`ChunkOptions`].
//! - [`stream`] — a native streaming [`CsvStreamIterator`].
//! - [`to_markdown`] — CSV → Markdown pipe table.

pub mod common;
mod chunker;
mod sliding_window;
mod stream;
mod to_markdown;

pub use stream::{stream, CsvStreamIterator};
pub use to_markdown::csv_to_markdown as to_markdown;
pub use to_markdown::csv_to_markdown_from_bytes as to_markdown_from_bytes;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::options::{ChunkMode, ChunkOptions};
use common::CsvChunkRecord;

fn to_chunks(records: Vec<CsvChunkRecord>) -> Vec<Chunk> {
    records
        .into_iter()
        .map(|r| Chunk::new(r.content, r.content_type, r.metadata))
        .collect()
}

/// Chunk a CSV/TSV file. `mode` is one of `row` | `default` | `sliding_window`
/// | `page_aware`.
#[allow(clippy::too_many_arguments)]
pub fn chunk(
    file_path: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<Vec<Chunk>> {
    let lower = file_path.to_ascii_lowercase();
    if !lower.ends_with(".csv") && !lower.ends_with(".tsv") {
        return Err(ChunkError::InvalidArg(format!(
            "Expected .csv or .tsv file path, got: {file_path}"
        )));
    }
    match mode {
        "row" | "default" | "sliding_window" | "page_aware" => {}
        _ => {
            return Err(ChunkError::InvalidArg(
                "mode must be 'row', 'default', 'sliding_window', or 'page_aware' for CSV".to_string(),
            ))
        }
    }
    if rows_per_chunk == 0 {
        return Err(ChunkError::InvalidArg("rows_per_chunk must be greater than 0".to_string()));
    }
    if window_size == 0 {
        return Err(ChunkError::InvalidArg("window_size must be greater than 0".to_string()));
    }
    if overlap >= window_size {
        return Err(ChunkError::InvalidArg("overlap must be less than window_size".to_string()));
    }
    let data = std::fs::read(file_path).map_err(ChunkError::Io)?;
    build_chunks(&data, mode, rows_per_chunk, window_size, overlap, include_headers, delimiter, encoding, skip_empty_rows)
}

/// No-filesystem entry (wasm/browser). `delimiter=None` auto-detects; for `.tsv`
/// the dispatch layer passes a tab delimiter.
#[allow(clippy::too_many_arguments)]
pub fn chunk_from_bytes(
    data: &[u8],
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<Vec<Chunk>> {
    match mode {
        "row" | "default" | "sliding_window" | "page_aware" => {}
        _ => return Err(ChunkError::InvalidArg("mode must be 'row', 'default', 'sliding_window', or 'page_aware' for CSV".to_string())),
    }
    build_chunks(data, mode, rows_per_chunk, window_size, overlap, include_headers, delimiter, encoding, skip_empty_rows)
}

#[allow(clippy::too_many_arguments)]
fn build_chunks(
    data: &[u8],
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<Vec<Chunk>> {
    let records = if mode == "page_aware" {
        chunker::build_page_aware_chunks(data, rows_per_chunk, include_headers, delimiter, encoding, skip_empty_rows)
    } else if mode == "sliding_window" {
        sliding_window::build_sliding_window_chunks(data, window_size, overlap, include_headers, delimiter, encoding, skip_empty_rows)
    } else {
        chunker::build_row_chunks(data, rows_per_chunk, include_headers, delimiter, encoding, skip_empty_rows)
    }
    .map_err(ChunkError::Parse)?;

    Ok(to_chunks(records))
}

/// Dispatch-layer entry: map a unified [`ChunkOptions`] onto CSV's strategies.
pub fn chunk_with_options(file_path: &str, opts: &ChunkOptions) -> Result<Vec<Chunk>> {
    let mode = match opts.mode {
        ChunkMode::Default | ChunkMode::Row => "row",
        ChunkMode::SlidingWindow => "sliding_window",
        ChunkMode::PageAware => "page_aware",
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "CSV does not support mode '{}'",
                other.as_str()
            )))
        }
    };
    chunk(
        file_path,
        mode,
        opts.rows_per_chunk,
        opts.window_size,
        opts.overlap,
        opts.include_headers,
        opts.delimiter,
        &opts.encoding,
        opts.skip_empty_rows,
    )
}
