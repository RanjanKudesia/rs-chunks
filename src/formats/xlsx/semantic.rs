use std::collections::HashSet;

use calamine::{Data, Reader};
use serde_json::json;

use super::common::{
    cell_to_string, detect_header_row, row_is_empty_public, serialize_row_kv,
    serialize_row_values_public, XlsxChunkRecord, CT_SEMANTIC,
};
use crate::shared::MAX_SEMANTIC_CHARS;

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

/// Serialize each row once, keeping its absolute row index alongside it.
///
/// Rows are serialized up front rather than inside the split loop so that
/// measuring a group against `MAX_SEMANTIC_CHARS` costs no extra serialization.
fn serialize_rows(
    group: &[(usize, Vec<Data>)],
    headers: &[String],
    include_headers: bool,
    col_count: usize,
) -> Vec<(usize, String)> {
    group
        .iter()
        .map(|(abs_row, row_cells)| {
            let text = if include_headers {
                serialize_row_kv(headers, row_cells)
            } else {
                serialize_row_values_public(row_cells, col_count)
            };
            (*abs_row, text)
        })
        .collect()
}

/// Split serialized rows into runs whose joined length stays within
/// `MAX_SEMANTIC_CHARS`, returning `(start, end)` index ranges.
///
/// Rows are never split internally: a single row longer than the cap becomes a
/// run of its own and exceeds it, exactly as an indivisible unit does in every
/// other semantic chunker. Without this, one category group serialized into a
/// single chunk with no upper bound at all (224,718 chars observed on
/// `xlsm/mv-calculator-final-2-20-2013.xlsm`), which no embedding model can
/// accept.
fn split_runs(rows: &[(usize, String)]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    let mut accum = 0usize;

    for (idx, (_, text)) in rows.iter().enumerate() {
        let len = text.chars().count();
        if idx > start {
            // +1 for the "\n" join separator.
            if accum + 1 + len > MAX_SEMANTIC_CHARS {
                runs.push((start, idx));
                start = idx;
                accum = len;
                continue;
            }
            accum += 1 + len;
        } else {
            accum = len;
        }
    }

    if start < rows.len() {
        runs.push((start, rows.len()));
    }
    runs
}

fn join_run(rows: &[(usize, String)]) -> String {
    rows.iter()
        .map(|(_, text)| text.as_str())
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

    let mut workbook = super::common::open_spreadsheet_from_bytes(data, ext)?;

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

        let category_column = detect_category_column(&data_rows);

        if let Some(cat_col) = category_column {
            // Sort so all rows sharing a category value are consecutive.
            data_rows.sort_by(|(_, a), (_, b)| {
                cell_to_string(a.get(cat_col).unwrap_or(&Data::Empty))
                    .cmp(&cell_to_string(b.get(cat_col).unwrap_or(&Data::Empty)))
            });

            // First pass: collect (serialized rows, category) per category group.
            // Rows are kept separate rather than pre-joined so an oversized
            // group can be split at a row boundary below.
            let mut raw_groups: Vec<(Vec<(usize, String)>, String)> = Vec::new();
            let mut current_group: Vec<(usize, Vec<Data>)> = Vec::new();
            let mut current_category = String::new();

            for (abs_row, cells) in data_rows {
                let category = cell_to_string(cells.get(cat_col).unwrap_or(&Data::Empty));
                if !current_group.is_empty() && category != current_category {
                    let rows_out =
                        serialize_rows(&current_group, &headers, include_headers, col_count);
                    raw_groups.push((rows_out, current_category.clone()));
                    current_group.clear();
                }
                if current_group.is_empty() {
                    current_category = category.clone();
                }
                current_group.push((abs_row, cells));
            }

            if !current_group.is_empty() {
                let rows_out = serialize_rows(&current_group, &headers, include_headers, col_count);
                raw_groups.push((rows_out, current_category));
            }

            // Grouping quality describes the *categories*, so it is computed
            // from the groups before any size-driven split.
            let total_rows: usize = raw_groups.iter().map(|(rows, _)| rows.len()).sum();
            let n_groups = raw_groups.len();
            let avg_group_size = if n_groups == 0 {
                0.0f64
            } else {
                total_rows as f64 / n_groups as f64
            };
            let low_grouping_quality = avg_group_size < 2.0;
            let avg_rounded = (avg_group_size * 100.0).round() / 100.0;

            for (grp_idx, (rows_out, category)) in raw_groups.into_iter().enumerate() {
                // A group larger than MAX_SEMANTIC_CHARS becomes several chunks
                // that all keep the same `group_index` — they are still one
                // semantic group, split only for size. Splitting never crosses a
                // category boundary.
                for (run_start, run_end) in split_runs(&rows_out) {
                    let run = &rows_out[run_start..run_end];
                    let start_row = run.first().map(|(i, _)| *i).unwrap_or(0);
                    let end_row = run.last().map(|(i, _)| *i).unwrap_or(start_row);

                    chunks.push(XlsxChunkRecord {
                        content: join_run(run),
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
                            "actual_row_count": run.len(),
                            "header_row": &headers,
                            "col_count": col_count,
                            "group_index": grp_idx,
                            "chunk_index": chunk_index,
                        }),
                    });
                    chunk_index += 1;
                }
            }
        } else {
            let mut idx = 0usize;
            let mut group_index = 0usize;
            while idx < data_rows.len() {
                let end = (idx + rows_per_chunk).min(data_rows.len());
                let rows_out =
                    serialize_rows(&data_rows[idx..end], &headers, include_headers, col_count);

                // The same bound applies here: `rows_per_chunk` caps the row
                // count, not the character count, so a wide sheet could still
                // produce an unbounded chunk without this split.
                for (run_start, run_end) in split_runs(&rows_out) {
                    let run = &rows_out[run_start..run_end];
                    let start_row = run.first().map(|(row, _)| *row).unwrap_or(0);
                    let end_row = run.last().map(|(row, _)| *row).unwrap_or(start_row);

                    chunks.push(XlsxChunkRecord {
                        content: join_run(run),
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
                            "actual_row_count": run.len(),
                            "header_row": &headers,
                            "col_count": col_count,
                            "group_index": group_index,
                            "chunk_index": chunk_index,
                        }),
                    });

                    chunk_index += 1;
                }

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
