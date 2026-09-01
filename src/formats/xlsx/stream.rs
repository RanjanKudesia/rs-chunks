//! Incremental streaming for spreadsheet chunking.
//!
//! `row` and `sliding_window` are genuine state machines: the workbook is
//! parsed once into `SheetData`, then **one** [`XlsxChunkRecord`] is built per
//! `next()`. The other four modes (`table`, `sheet`, `page_aware`, `semantic`)
//! need global analysis of the whole sheet before any chunk is correct, so they
//! batch-drain — all chunks built up front, yielded one at a time.
//!
//! **Parity guarantee:** every mode yields byte-identical content and metadata
//! to the corresponding batch builder. `stream_matches_batch_for_every_mode` in
//! `tests/xlsx_stream.rs` asserts exactly that over the fixture corpus, because
//! the two produce their chunks by completely different code paths and nothing
//! else would catch them drifting.
//!
//! This is the only genuinely lazy chunking path in the engine. It lived in
//! py_chunks' binding until 2026-08-05, which meant one of three SDKs had it by
//! accident; moving it here gives all three the same behaviour (see
//! CONSOLIDATION_PLAN.md "DECIDED 2026-08-05").

use calamine::{Data, Reader};
use serde_json::json;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};

use super::common::{
    cell_to_string, data_start_with_header_fallback, detect_header_row,
    open_spreadsheet_from_bytes, read_worksheet_range, row_is_empty_public, serialize_row_kv,
    serialize_row_values_public, XlsxChunkRecord, CT_ROW, CT_SLIDING_WINDOW,
};

// ── Pre-parsed sheet data (row / sliding_window state machines) ───────────────

struct SheetData {
    sheet_name: String,
    sheet_index: usize,
    headers: Vec<String>,
    col_count: usize,
    /// (absolute_row_index, owned_cells_padded_to_col_count)
    data_rows: Vec<(usize, Vec<Data>)>,
}

fn row_slice_owned(row: &[Data], col_count: usize) -> Vec<Data> {
    (0..col_count)
        .map(|i| row.get(i).cloned().unwrap_or(Data::Empty))
        .collect()
}

fn build_headers_from_rows(
    rows: &[&[Data]],
    header_row_index: Option<usize>,
    col_count: usize,
) -> Vec<String> {
    (0..col_count)
        .map(|idx| {
            let h = header_row_index
                .and_then(|ri| rows.get(ri))
                .and_then(|row| row.get(idx))
                .map(cell_to_string)
                .unwrap_or_default();
            if h.trim().is_empty() {
                format!("Column {}", idx + 1)
            } else {
                h
            }
        })
        .collect()
}

/// Open the workbook once and collect all row data per sheet. Shared by the
/// `row` and `sliding_window` state machines.
fn parse_sheets_for_streaming(
    data: &[u8],
    ext: &str,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
) -> std::result::Result<(Vec<SheetData>, Vec<String>), String> {
    let mut workbook = open_spreadsheet_from_bytes(data, ext)?;

    let workbook_sheet_names = workbook.sheet_names().to_vec();
    let selected_sheets = if sheet_names.is_empty() {
        workbook_sheet_names.clone()
    } else {
        for name in &sheet_names {
            if !workbook_sheet_names.iter().any(|n| n == name) {
                return Err(format!("Sheet '{name}' not found"));
            }
        }
        sheet_names
    };

    let mut result = Vec::new();
    let mut readable_sheets = 0usize;
    let mut first_sheet_error: Option<String> = None;
    // Streaming must report the same skipped sheets the batch path does, or the
    // two disagree about the same workbook. (#66)
    let mut skipped_sheets: Vec<String> = Vec::new();
    for sheet_name in selected_sheets {
        let sheet_index = workbook_sheet_names
            .iter()
            .position(|n| n == &sheet_name)
            .unwrap_or(0);

        // A sheet calamine cannot read (chart sheets, XLM macro sheets) must not
        // take the whole workbook down with it — skip it and keep going.
        let range = match read_worksheet_range(&mut workbook, &sheet_name) {
            Ok(range) => {
                readable_sheets += 1;
                range
            }
            Err(e) => {
                first_sheet_error.get_or_insert(e);
                skipped_sheets.push(sheet_name.clone());
                continue;
            }
        };
        let base_row_index = range.start().map(|(r, _)| r as usize).unwrap_or(0);

        let rows: Vec<&[Data]> = range.rows().collect();
        if rows.is_empty() {
            continue;
        }

        let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }

        let header_row_index = detect_header_row(&rows);
        let headers = build_headers_from_rows(&rows, header_row_index, col_count);
        // Shared with every batch builder, so the two paths cannot drift — they
        // did, and that was TECH_DEBT #80.
        let data_start = data_start_with_header_fallback(&rows, header_row_index, skip_empty_rows);

        let mut data_rows: Vec<(usize, Vec<Data>)> = Vec::new();
        for (row_index, row) in rows.iter().enumerate().skip(data_start) {
            let cells = row_slice_owned(row, col_count);
            if skip_empty_rows && row_is_empty_public(&cells) {
                continue;
            }
            data_rows.push((base_row_index + row_index, cells));
        }

        if data_rows.is_empty() {
            continue;
        }

        result.push(SheetData {
            sheet_name,
            sheet_index,
            headers,
            col_count,
            data_rows,
        });
    }

    // Every selected sheet failed to read: this is not an empty workbook,
    // it is an unreadable one — surface the first failure rather than
    // returning success with no chunks.
    if readable_sheets == 0 {
        if let Some(e) = first_sheet_error {
            return Err(e);
        }
    }
    Ok((result, skipped_sheets))
}

// ── Row state machine ─────────────────────────────────────────────────────────

struct RowStreamState {
    sheets: Vec<SheetData>,
    skipped_sheets: Vec<String>,
    sheet_idx: usize,
    row_cursor: usize,
    rows_per_chunk: usize,
    include_headers: bool,
    chunk_index: usize,
}

impl RowStreamState {
    fn advance(&mut self) -> Option<XlsxChunkRecord> {
        loop {
            if self.sheet_idx >= self.sheets.len() {
                return None;
            }

            let sheet_len = self.sheets[self.sheet_idx].data_rows.len();
            if self.row_cursor >= sheet_len {
                self.sheet_idx += 1;
                self.row_cursor = 0;
                self.chunk_index = 0; // build_row_chunks resets chunk_index per sheet
                continue;
            }

            let end = (self.row_cursor + self.rows_per_chunk).min(sheet_len);

            // Collect chunk data while sheet is borrowed, then mutate self.
            let (
                content,
                first_row_index,
                actual_row_count,
                sheet_name,
                sheet_index,
                headers,
                col_count,
            ) = {
                let sheet = &self.sheets[self.sheet_idx];
                let group = &sheet.data_rows[self.row_cursor..end];
                let include_headers = self.include_headers;
                let content = group
                    .iter()
                    .map(|(_, cells)| {
                        if include_headers {
                            serialize_row_kv(&sheet.headers, cells)
                        } else {
                            serialize_row_values_public(cells, sheet.col_count)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let first_row_index = group[0].0;
                let actual_row_count = group.len();
                (
                    content,
                    first_row_index,
                    actual_row_count,
                    sheet.sheet_name.clone(),
                    sheet.sheet_index,
                    sheet.headers.clone(),
                    sheet.col_count,
                )
            };

            let chunk_index = self.chunk_index;
            let rows_per_chunk = self.rows_per_chunk;
            self.row_cursor = end;
            self.chunk_index += 1;

            return Some(XlsxChunkRecord {
                content,
                content_type: CT_ROW.to_string(),
                metadata: json!({
                    "sheet_name": sheet_name,
                    "sheet_index": sheet_index,
                    "row_index": first_row_index,
                    "header_row": headers,
                    "col_count": col_count,
                    "rows_per_chunk": rows_per_chunk,
                    "actual_row_count": actual_row_count,
                    "chunk_index": chunk_index,
                    "skipped_sheets": self.skipped_sheets.clone(),
                }),
            });
        }
    }
}

// ── Sliding-window state machine ──────────────────────────────────────────────

struct SlidingWindowStreamState {
    sheets: Vec<SheetData>,
    skipped_sheets: Vec<String>,
    sheet_idx: usize,
    window_start: usize,
    window_index: usize, // resets per sheet (mirrors build_sliding_window_chunks)
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    chunk_index: usize,
}

impl SlidingWindowStreamState {
    fn advance(&mut self) -> Option<XlsxChunkRecord> {
        let step = self.window_size - self.overlap;
        loop {
            if self.sheet_idx >= self.sheets.len() {
                return None;
            }

            let sheet_len = self.sheets[self.sheet_idx].data_rows.len();
            if self.window_start >= sheet_len {
                self.sheet_idx += 1;
                self.window_start = 0;
                self.window_index = 0;
                continue;
            }

            let end = (self.window_start + self.window_size).min(sheet_len);

            let (
                content,
                start_row,
                end_row,
                actual_row_count,
                sheet_name,
                sheet_index,
                headers,
                col_count,
            ) = {
                let sheet = &self.sheets[self.sheet_idx];
                let window = &sheet.data_rows[self.window_start..end];
                let include_headers = self.include_headers;
                let content = window
                    .iter()
                    .map(|(_, cells)| {
                        if include_headers {
                            serialize_row_kv(&sheet.headers, cells)
                        } else {
                            serialize_row_values_public(cells, sheet.col_count)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let start_row = window.first().map(|(i, _)| *i).unwrap_or(0);
                let end_row = window.last().map(|(i, _)| *i).unwrap_or(start_row);
                let actual_row_count = window.len();
                (
                    content,
                    start_row,
                    end_row,
                    actual_row_count,
                    sheet.sheet_name.clone(),
                    sheet.sheet_index,
                    sheet.headers.clone(),
                    sheet.col_count,
                )
            };

            let chunk_index = self.chunk_index;
            let window_index = self.window_index;
            let window_size = self.window_size;
            let overlap = self.overlap;
            self.window_start += step;
            self.window_index += 1;
            self.chunk_index += 1;

            return Some(XlsxChunkRecord {
                content,
                content_type: CT_SLIDING_WINDOW.to_string(),
                metadata: json!({
                    "sheet_name": sheet_name,
                    "sheet_index": sheet_index,
                    "window_size": window_size,
                    "overlap": overlap,
                    "actual_row_count": actual_row_count,
                    "window_index": window_index,
                    "start_row": start_row,
                    "end_row": end_row,
                    "header_row": headers,
                    "col_count": col_count,
                    "chunk_index": chunk_index,
                    "skipped_sheets": self.skipped_sheets.clone(),
                }),
            });
        }
    }
}

// ── Batch-drain (table / sheet / page_aware / semantic) ───────────────────────

struct BatchDrainState {
    chunks: std::vec::IntoIter<XlsxChunkRecord>,
}

// ── Backend ───────────────────────────────────────────────────────────────────

enum Backend {
    Row(Box<RowStreamState>),
    SlidingWindow(Box<SlidingWindowStreamState>),
    Batch(BatchDrainState),
}

/// Lazy iterator over spreadsheet chunks. See the module docs for which modes
/// are incremental and which batch-drain.
pub struct XlsxChunkStream {
    backend: Backend,
}

impl Iterator for XlsxChunkStream {
    type Item = Result<Chunk>;

    fn next(&mut self) -> Option<Self::Item> {
        let record = match &mut self.backend {
            Backend::Row(s) => s.advance(),
            Backend::SlidingWindow(s) => s.advance(),
            Backend::Batch(s) => s.chunks.next(),
        }?;
        Some(Ok(Chunk::new(
            record.content,
            record.content_type,
            record.metadata,
        )))
    }
}

/// Reject the argument combinations a mode cannot use. Mirrors the batch
/// entry points so streaming and batch refuse the same inputs.
fn validate(mode: &str, rows_per_chunk: usize, max_chunk_chars: usize) -> Result<()> {
    if matches!(mode, "row" | "default" | "semantic") && rows_per_chunk < 1 {
        return Err(ChunkError::InvalidArg(
            "rows_per_chunk must be greater than 0".into(),
        ));
    }
    if matches!(mode, "table" | "sheet" | "page_aware") && max_chunk_chars < 1 {
        return Err(ChunkError::InvalidArg(
            "max_chunk_chars must be greater than 0".into(),
        ));
    }
    Ok(())
}

/// `Sheet '…' not found` is a caller error; anything else is a parse failure.
/// The distinction is part of the published contract (py_chunks maps the first
/// to `ValueError` and the second to `RuntimeError`).
pub(super) fn build_err(err: String) -> ChunkError {
    if err.starts_with("Sheet '") && err.ends_with("' not found") {
        ChunkError::InvalidArg(err)
    } else {
        ChunkError::Parse(err)
    }
}

/// Build a lazy chunk stream from spreadsheet bytes. `ext` (e.g. `"xlsx"`,
/// `"ods"`) routes calamine's format detection and the ODS mimetype repair.
#[allow(clippy::too_many_arguments)]
pub fn stream_from_bytes(
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
) -> Result<XlsxChunkStream> {
    validate(mode, rows_per_chunk, max_chunk_chars)?;

    let backend = match mode {
        "row" | "default" => {
            let (sheets, skipped_sheets) =
                parse_sheets_for_streaming(data, ext, sheet_names, skip_empty_rows)
                    .map_err(build_err)?;
            Backend::Row(Box::new(RowStreamState {
                sheets,
                skipped_sheets,
                sheet_idx: 0,
                row_cursor: 0,
                rows_per_chunk,
                include_headers,
                chunk_index: 0,
            }))
        }
        "sliding_window" => {
            if window_size < 1 {
                return Err(ChunkError::InvalidArg(
                    "window_size must be greater than 0".into(),
                ));
            }
            if overlap >= window_size {
                return Err(ChunkError::InvalidArg(
                    "overlap must be less than window_size".into(),
                ));
            }
            let (sheets, skipped_sheets) =
                parse_sheets_for_streaming(data, ext, sheet_names, skip_empty_rows)
                    .map_err(build_err)?;
            Backend::SlidingWindow(Box::new(SlidingWindowStreamState {
                sheets,
                skipped_sheets,
                sheet_idx: 0,
                window_start: 0,
                window_index: 0,
                window_size,
                overlap,
                include_headers,
                chunk_index: 0,
            }))
        }
        "table" => Backend::Batch(BatchDrainState {
            chunks: super::table_region::build_table_chunks(
                data,
                ext,
                include_headers,
                sheet_names,
                skip_empty_rows,
                max_chunk_chars,
            )
            .map_err(build_err)?
            .into_iter(),
        }),
        "sheet" => Backend::Batch(BatchDrainState {
            chunks: super::sheet::build_sheet_chunks(
                data,
                ext,
                include_headers,
                sheet_names,
                skip_empty_rows,
                max_chunk_chars,
            )
            .map_err(build_err)?
            .into_iter(),
        }),
        "page_aware" => Backend::Batch(BatchDrainState {
            chunks: super::page_aware::build_page_aware_chunks(
                data,
                ext,
                include_headers,
                sheet_names,
                skip_empty_rows,
                max_chunk_chars,
            )
            .map_err(build_err)?
            .into_iter(),
        }),
        "semantic" => Backend::Batch(BatchDrainState {
            chunks: super::semantic::build_semantic_chunks(
                data,
                ext,
                rows_per_chunk,
                include_headers,
                sheet_names,
                skip_empty_rows,
            )
            .map_err(build_err)?
            .into_iter(),
        }),
        other => {
            return Err(ChunkError::InvalidArg(format!(
                "Unknown XLSX streaming mode: {other}"
            )))
        }
    };

    Ok(XlsxChunkStream { backend })
}
