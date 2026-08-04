
use calamine::{Data, Reader};
use serde_json::json;

use super::common::{
    cell_to_string, detect_header_row, get_named_table_names_for_sheet, row_is_empty_public,
    serialize_row_kv, serialize_row_values_public, split_content_lines, XlsxChunkRecord, CT_SHEET,
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

pub fn build_sheet_chunks(
    data: &[u8],
    ext: &str,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
    max_chunk_chars: usize,
) -> Result<Vec<XlsxChunkRecord>, String> {
    if max_chunk_chars == 0 {
        return Err("max_chunk_chars must be > 0".to_string());
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

    let mut readable_sheets = 0usize;
    let mut first_sheet_error: Option<String> = None;
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
                continue;
            }
        };

        let rows: Vec<&[Data]> = range.rows().collect();
        if rows.is_empty() {
            continue;
        }
        if rows.iter().all(|row| row_is_empty_public(row)) {
            continue;
        }

        let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }

        let header_row_index = detect_header_row(&rows);
        let headers = build_headers(&rows, header_row_index, col_count);
        let data_start_row = header_row_index.map_or(0, |idx| idx + 1);

        let mut data_lines: Vec<String> = Vec::new();
        for row in rows.iter().skip(data_start_row) {
            let values = row_slice_with_fill(row, col_count);
            if skip_empty_rows && row_is_empty_public(&values) {
                continue;
            }
            let line = if include_headers {
                serialize_row_kv(&headers, &values)
            } else {
                serialize_row_values_public(&values, col_count)
            };
            data_lines.push(line);
        }

        // F2 guard: if the only content was consumed as a header (e.g. a single
        // merged title cell), don't emit an empty sheet — surface that content.
        if data_lines.is_empty() {
            if let Some(hrow) = header_row_index.and_then(|idx| rows.get(idx)) {
                let values = row_slice_with_fill(hrow, col_count);
                if !row_is_empty_public(&values) {
                    data_lines.push(serialize_row_values_public(&values, col_count));
                }
            }
        }

        let table_names =
            get_named_table_names_for_sheet(data, ext, sheet_index + 1, &sheet_name)?;
        let split_parts = split_content_lines(data_lines, max_chunk_chars);
        let is_split = split_parts.len() > 1;

        for (split_part, part) in split_parts.into_iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            let row_count = part.len();
            let content = part.join("\n");

            chunks.push(XlsxChunkRecord {
                content,
                content_type: CT_SHEET.to_string(),
                metadata: json!({
                    "sheet_name": sheet_name,
                    "sheet_index": sheet_index,
                    "row_count": row_count,
                    "col_count": col_count,
                    "header_row": &headers,
                    "has_named_tables": !table_names.is_empty(),
                    "named_tables": &table_names,
                    "chunk_index": chunk_index,
                    "is_split": is_split,
                    "split_part": if is_split { Some(split_part) } else { None::<usize> },
                }),
            });
            chunk_index += 1;
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

    Ok(chunks)
}

