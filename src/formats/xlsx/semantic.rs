use std::collections::HashSet;

use calamine::{Data, Reader};
use serde_json::json;

use super::common::{
    cell_to_string, detect_header_row, row_is_empty_public, serialize_row_kv,
    serialize_row_values_public, XlsxChunkRecord, CT_SEMANTIC,
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

fn detect_category_column(data_rows: &[(usize, Vec<Data>)]) -> Option<usize> {
    let col_count = data_rows
        .iter()
        .map(|(_, cells)| cells.len())
        .max()
        .unwrap_or(0);

    let mut best_col: Option<usize> = None;
    let mut best_cardinality = usize::MAX;

    for col_idx in 0..col_count {
        let values: Vec<String> = data_rows
            .iter()
            .filter_map(|(_, cells)| match cells.get(col_idx) {
                Some(Data::String(s)) if !s.trim().is_empty() => Some(s.clone()),
                _ => None,
            })
            .collect();

        if values.len() < 2 {
            continue;
        }

        let unique: HashSet<&str> = values.iter().map(|s| s.as_str()).collect();
        let cardinality = unique.len();

        if cardinality < values.len() && cardinality < best_cardinality {
            best_cardinality = cardinality;
            best_col = Some(col_idx);
        }
    }

    best_col
}

fn serialize_group(
    group: &[(usize, Vec<Data>)],
    headers: &[String],
    include_headers: bool,
    col_count: usize,
) -> String {
    group
        .iter()
        .map(|(_, row_cells)| {
            if include_headers {
                serialize_row_kv(headers, row_cells)
            } else {
                serialize_row_values_public(row_cells, col_count)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn build_semantic_chunks(
    data: &[u8],
    ext: &str,
    rows_per_chunk: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
) -> Result<Vec<XlsxChunkRecord>, String> {
    if rows_per_chunk == 0 {
        return Err("rows_per_chunk must be > 0".to_string());
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
        let data_start_row = header_row_index.map_or(0, |idx| idx + 1);

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

        let category_column = detect_category_column(&data_rows);

        if let Some(cat_col) = category_column {
            // Sort so all rows sharing a category value are consecutive.
            data_rows.sort_by(|(_, a), (_, b)| {
                cell_to_string(a.get(cat_col).unwrap_or(&Data::Empty))
                    .cmp(&cell_to_string(b.get(cat_col).unwrap_or(&Data::Empty)))
            });

            // First pass: collect (content, category, start_row, end_row, row_count).
            let mut raw_groups: Vec<(String, String, usize, usize, usize)> = Vec::new();
            let mut current_group: Vec<(usize, Vec<Data>)> = Vec::new();
            let mut current_category = String::new();

            for (abs_row, cells) in data_rows {
                let category = cell_to_string(cells.get(cat_col).unwrap_or(&Data::Empty));
                if !current_group.is_empty() && category != current_category {
                    let start_row = current_group.first().map(|(i, _)| *i).unwrap_or(0);
                    let end_row = current_group.last().map(|(i, _)| *i).unwrap_or(start_row);
                    let content = serialize_group(&current_group, &headers, include_headers, col_count);
                    raw_groups.push((content, current_category.clone(), start_row, end_row, current_group.len()));
                    current_group.clear();
                }
                if current_group.is_empty() {
                    current_category = category.clone();
                }
                current_group.push((abs_row, cells));
            }

            if !current_group.is_empty() {
                let start_row = current_group.first().map(|(i, _)| *i).unwrap_or(0);
                let end_row = current_group.last().map(|(i, _)| *i).unwrap_or(start_row);
                let content = serialize_group(&current_group, &headers, include_headers, col_count);
                raw_groups.push((content, current_category, start_row, end_row, current_group.len()));
            }

            // Compute grouping quality before emitting chunks.
            let total_rows: usize = raw_groups.iter().map(|(_, _, _, _, n)| *n).sum();
            let n_groups = raw_groups.len();
            let avg_group_size = if n_groups == 0 {
                0.0f64
            } else {
                total_rows as f64 / n_groups as f64
            };
            let low_grouping_quality = avg_group_size < 2.0;
            let avg_rounded = (avg_group_size * 100.0).round() / 100.0;

            for (grp_idx, (content, category, start_row, end_row, row_count)) in
                raw_groups.into_iter().enumerate()
            {
                chunks.push(XlsxChunkRecord {
                    content,
                    content_type: CT_SEMANTIC.to_string(),
                    metadata: json!({
                        "sheet_name": sheet_name,
                        "sheet_index": sheet_index,
                        "category_column": cat_col,
                        "category_value": category,
                        "used_fallback": false,
                        "low_grouping_quality": low_grouping_quality,
                        "avg_group_size": avg_rounded,
                        "start_row": start_row,
                        "end_row": end_row,
                        "actual_row_count": row_count,
                        "header_row": &headers,
                        "col_count": col_count,
                        "group_index": grp_idx,
                        "chunk_index": chunk_index,
                    }),
                });
                chunk_index += 1;
            }
        } else {
            let mut idx = 0usize;
            let mut group_index = 0usize;
            while idx < data_rows.len() {
                let end = (idx + rows_per_chunk).min(data_rows.len());
                let group = &data_rows[idx..end];

                let start_row = group.first().map(|(row, _)| *row).unwrap_or(0);
                let end_row = group.last().map(|(row, _)| *row).unwrap_or(start_row);
                let content = serialize_group(group, &headers, include_headers, col_count);

                chunks.push(XlsxChunkRecord {
                    content,
                    content_type: CT_SEMANTIC.to_string(),
                    metadata: json!({
                        "sheet_name": sheet_name,
                        "sheet_index": sheet_index,
                        "category_column": Option::<usize>::None,
                        "category_value": Option::<String>::None,
                        "used_fallback": true,
                        "low_grouping_quality": false,
                        "avg_group_size": 0.0f64,
                        "start_row": start_row,
                        "end_row": end_row,
                        "actual_row_count": group.len(),
                        "header_row": &headers,
                        "col_count": col_count,
                        "group_index": group_index,
                        "chunk_index": chunk_index,
                    }),
                });

                chunk_index += 1;
                group_index += 1;
                idx = end;
            }
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

