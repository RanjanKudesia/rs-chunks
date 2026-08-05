
use calamine::{Data, Reader};
use serde_json::json;

use super::common::{
    cell_to_string, detect_header_row, row_is_empty_public, serialize_row_kv,
    serialize_row_values_public, XlsxChunkRecord, CT_SLIDING_WINDOW,
};

fn row_slice_with_fill(row: &[Data], col_count: usize) -> Vec<Data> {
    (0..col_count)
        .map(|idx| row.get(idx).cloned().unwrap_or(Data::Empty))
        .collect()
}

fn build_headers(
    rows: &[&[Data]],
    header_row_index: Option<usize>,
    col_count: usize,
) -> Vec<String> {
    let mut headers = Vec::with_capacity(col_count);
    for idx in 0..col_count {
        let header = header_row_index
            .and_then(|row_index| rows.get(row_index))
            .and_then(|row| row.get(idx))
            .map(cell_to_string)
            .unwrap_or_default();
        if header.trim().is_empty() {
            headers.push(format!("Column {}", idx + 1));
        } else {
            headers.push(header);
        }
    }
    headers
}

pub fn build_sliding_window_chunks(
    data: &[u8],
    ext: &str,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
) -> Result<Vec<XlsxChunkRecord>, String> {
    if window_size == 0 {
        return Err("window_size must be >= 1".to_string());
    }
    if overlap >= window_size {
        return Err("overlap must be < window_size".to_string());
    }

    let mut workbook =
        super::common::open_spreadsheet_from_bytes(data, ext)?;

    let workbook_sheet_names = workbook.sheet_names().to_vec();
    let selected_sheets = if sheet_names.is_empty() {
        workbook_sheet_names.clone()
    } else {
        for sheet_name in &sheet_names {
            if !workbook_sheet_names.iter().any(|name| name == sheet_name) {
                return Err(format!("Sheet '{sheet_name}' not found"));
            }
        }
        sheet_names
    };

    let mut chunks = Vec::new();
    let mut chunk_index = 0usize;
    let step = window_size - overlap;

    let mut readable_sheets = 0usize;
    let mut first_sheet_error: Option<String> = None;
    let mut skipped_sheets: Vec<String> = Vec::new();
    for sheet_name in selected_sheets {
        let sheet_index = workbook_sheet_names
            .iter()
            .position(|name| name == &sheet_name)
            .unwrap_or(0);

        // A sheet calamine cannot read (chart sheets, XLM macro sheets) must not
        // take the whole workbook down with it — skip it and keep going.
        let range = match super::common::read_worksheet_range(&mut workbook, &sheet_name) {
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
        let base_row_index = range.start().map(|(row, _)| row as usize).unwrap_or(0);

        let rows: Vec<&[Data]> = range.rows().collect();
        if rows.is_empty() {
            continue;
        }

        let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }

        let header_row_index = detect_header_row(&rows);
        let headers = build_headers(&rows, header_row_index, col_count);
        let data_start_row = super::common::data_start_with_header_fallback(
            &rows,
            header_row_index,
            skip_empty_rows,
        );

        let mut data_rows: Vec<(usize, Vec<Data>)> = Vec::new();
        for (row_index, row) in rows.iter().enumerate().skip(data_start_row) {
            let values = row_slice_with_fill(row, col_count);
            if skip_empty_rows && row_is_empty_public(&values) {
                continue;
            }
            data_rows.push((base_row_index + row_index, values));
        }

        if data_rows.is_empty() {
            continue;
        }

        let mut start = 0usize;
        let mut window_index = 0usize;
        while start < data_rows.len() {
            let end = (start + window_size).min(data_rows.len());
            let window = &data_rows[start..end];

            let content = window
                .iter()
                .map(|(_, cells)| {
                    if include_headers {
                        serialize_row_kv(&headers, cells)
                    } else {
                        serialize_row_values_public(cells, col_count)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            let start_row = window.first().map(|(idx, _)| *idx).unwrap_or(0);
            let end_row = window.last().map(|(idx, _)| *idx).unwrap_or(start_row);

            chunks.push(XlsxChunkRecord {
                content,
                content_type: CT_SLIDING_WINDOW.to_string(),
                metadata: json!({
                    "sheet_name": sheet_name,
                    "sheet_index": sheet_index,
                    "window_size": window_size,
                    "overlap": overlap,
                    "actual_row_count": window.len(),
                    "window_index": window_index,
                    "start_row": start_row,
                    "end_row": end_row,
                    "header_row": headers,
                    "col_count": col_count,
                    "chunk_index": chunk_index,
                }),
            });

            chunk_index += 1;
            window_index += 1;
            start += step;
        }
    }
    // Every selected sheet failed to read: this is not an empty workbook,
    // it is an unreadable one — surface the first failure rather than
    // returning success with no chunks.
    if readable_sheets == 0 {
        if let Some(e) = first_sheet_error {
            return Err(e);
        }
    }

    super::common::stamp_skipped_sheets(&mut chunks, &skipped_sheets);
    Ok(chunks)
}

