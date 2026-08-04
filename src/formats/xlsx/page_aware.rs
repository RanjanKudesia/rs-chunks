use crate::entities::read_event_folding_entities;
use std::collections::HashMap;
use std::io::Read;

use calamine::{Data, Reader};
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use serde_json::json;
use zip::ZipArchive;

use super::common::{
    cell_to_string, detect_header_row, parse_range_ref, row_is_empty_public, serialize_row_kv,
    serialize_row_values_public, split_content_lines, XlsxChunkRecord, CT_PAGE_AWARE,
};

fn clean_range_ref(raw: &str) -> Option<String> {
    let after_exclaim = raw.trim().split('!').last()?;
    let clean = after_exclaim.replace('$', "");
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn col_index_to_letter(mut idx: usize) -> String {
    idx += 1;
    let mut out = String::new();
    while idx > 0 {
        let rem = (idx - 1) % 26;
        out.insert(0, (b'A' + rem as u8) as char);
        idx = (idx - 1) / 26;
    }
    out
}

fn range_ref_from_indices(
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
) -> String {
    format!(
        "{}{}:{}{}",
        col_index_to_letter(start_col),
        start_row + 1,
        col_index_to_letter(end_col),
        end_row + 1
    )
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

fn row_slice_with_fill(row: &[Data], start_col: usize, end_col: usize) -> Vec<Data> {
    (start_col..=end_col)
        .map(|idx| row.get(idx).cloned().unwrap_or(Data::Empty))
        .collect()
}

fn parse_print_areas_for_workbook(
    data: &[u8],
) -> Result<HashMap<usize, Vec<(usize, usize, usize, usize)>>, String> {
    let mut archive = match ZipArchive::new(std::io::Cursor::new(data.to_vec())) {
        Ok(a) => a,
        Err(_) => return Ok(HashMap::new()), // Not a ZIP archive (e.g. XLS) — no print areas
    };

    let mut workbook_xml = match archive.by_name("xl/workbook.xml") {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(HashMap::new()),
        Err(e) => {
            return Err(format!(
                "Failed to open 'xl/workbook.xml' in xlsx archive: {e}"
            ))
        }
    };

    let mut bytes = Vec::new();
    workbook_xml
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read 'xl/workbook.xml': {e}"))?;

    let mut reader = XmlReader::from_reader(bytes.as_slice());
    let mut buf = Vec::new();

    let mut in_print_area_defined_name = false;
    let mut current_local_sheet_id: Option<usize> = None;
    let mut text_buf = String::new();
    let mut out: HashMap<usize, Vec<(usize, usize, usize, usize)>> = HashMap::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse workbook XML: {e}")),
            Ok(Event::Start(ref e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let local = name_bytes
                    .rsplit(|b| *b == b':')
                    .next()
                    .unwrap_or(name_bytes.as_slice());
                if local == b"definedName" {
                    let mut is_print_area = false;
                    let mut local_sheet_id = None;
                    for attr in e.attributes().flatten() {
                        let key_bytes = attr.key.as_ref().to_vec();
                        let key_local = key_bytes
                            .rsplit(|b| *b == b':')
                            .next()
                            .unwrap_or(key_bytes.as_slice());
                        let value = String::from_utf8_lossy(attr.value.as_ref()).into_owned();
                        if key_local == b"name" && value == "_xlnm.Print_Area" {
                            is_print_area = true;
                        } else if key_local == b"localSheetId" {
                            local_sheet_id = value.parse::<usize>().ok();
                        }
                    }
                    if is_print_area {
                        in_print_area_defined_name = true;
                        current_local_sheet_id = local_sheet_id;
                        text_buf.clear();
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_print_area_defined_name {
                    text_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Ok(Event::End(ref e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let local = name_bytes
                    .rsplit(|b| *b == b':')
                    .next()
                    .unwrap_or(name_bytes.as_slice());
                if in_print_area_defined_name && local == b"definedName" {
                    if let Some(sheet_idx) = current_local_sheet_id {
                        for raw_part in text_buf.split(',') {
                            let Some(clean) = clean_range_ref(raw_part) else {
                                continue;
                            };
                            let Some((sr, sc, er, ec)) = parse_range_ref(&clean) else {
                                continue;
                            };
                            out.entry(sheet_idx).or_default().push((sr, sc, er, ec));
                        }
                    }
                    in_print_area_defined_name = false;
                    current_local_sheet_id = None;
                    text_buf.clear();
                }
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(out)
}

pub fn build_page_aware_chunks(
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
    let print_areas = parse_print_areas_for_workbook(data)?;

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
        let (base_row, base_col) = range
            .start()
            .map(|(r, c)| (r as usize, c as usize))
            .unwrap_or((0, 0));

        let rows: Vec<&[Data]> = range.rows().collect();
        if rows.is_empty() {
            continue;
        }

        let maybe_areas = print_areas.get(&sheet_index);
        if let Some(areas) = maybe_areas {
            let mut region_index = 0usize;
            for (start_row, start_col, end_row, end_col) in areas {
                if start_col > end_col || start_row > end_row {
                    continue;
                }

                let local_row_start = start_row.saturating_sub(base_row);
                let local_row_end = end_row.saturating_sub(base_row);
                let local_col_start = start_col.saturating_sub(base_col);
                let local_col_end = end_col.saturating_sub(base_col);

                if local_row_start >= rows.len()
                    || local_row_end >= rows.len()
                    || local_row_start > local_row_end
                {
                    continue;
                }

                let col_count = end_col - start_col + 1;
                let region_rows_owned: Vec<Vec<Data>> = (local_row_start..=local_row_end)
                    .map(|rid| row_slice_with_fill(rows[rid], local_col_start, local_col_end))
                    .collect();
                let region_row_refs: Vec<&[Data]> =
                    region_rows_owned.iter().map(|r| r.as_slice()).collect();

                let header_row_index = detect_header_row(&region_row_refs);
                let headers = build_headers(&region_row_refs, header_row_index, col_count);
                let data_start = header_row_index.map_or(0, |idx| idx + 1);

                let mut data_lines = Vec::new();
                for row in region_rows_owned.iter().skip(data_start) {
                    if skip_empty_rows && row_is_empty_public(row) {
                        continue;
                    }
                    let line = if include_headers {
                        serialize_row_kv(&headers, row)
                    } else {
                        serialize_row_values_public(row, col_count)
                    };
                    data_lines.push(line);
                }

                let print_area_ref =
                    range_ref_from_indices(*start_row, *start_col, *end_row, *end_col);
                let split_parts = split_content_lines(data_lines, max_chunk_chars);
                let is_split = split_parts.len() > 1;

                for (split_part, part) in split_parts.into_iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    let row_count = part.len();
                    chunks.push(XlsxChunkRecord {
                        content: part.join("\n"),
                        content_type: CT_PAGE_AWARE.to_string(),
                        metadata: json!({
                            "sheet_name": sheet_name,
                            "sheet_index": sheet_index,
                            "has_print_area": true,
                            "print_area_ref": print_area_ref,
                            "start_row": start_row,
                            "end_row": end_row,
                            "start_col": start_col,
                            "end_col": end_col,
                            "row_count": row_count,
                            "col_count": col_count,
                            "header_row": &headers,
                            "region_index": region_index,
                            "chunk_index": chunk_index,
                            "is_split": is_split,
                            "split_part": if is_split { Some(split_part) } else { None::<usize> },
                        }),
                    });
                    chunk_index += 1;
                }
                region_index += 1;
            }
        } else {
            if rows.iter().all(|row| row_is_empty_public(row)) {
                continue;
            }

            let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
            if col_count == 0 {
                continue;
            }

            let header_row_index = detect_header_row(&rows);
            let headers = build_headers(&rows, header_row_index, col_count);
            let data_start = header_row_index.map_or(0, |idx| idx + 1);

            let mut data_lines = Vec::new();
            for row in rows.iter().skip(data_start) {
                let filled = row_slice_with_fill(row, 0, col_count - 1);
                if skip_empty_rows && row_is_empty_public(&filled) {
                    continue;
                }
                let line = if include_headers {
                    serialize_row_kv(&headers, &filled)
                } else {
                    serialize_row_values_public(&filled, col_count)
                };
                data_lines.push(line);
            }

            let start_row = base_row;
            let end_row = base_row + rows.len().saturating_sub(1);
            let start_col = base_col;
            let end_col = base_col + col_count.saturating_sub(1);

            let split_parts = split_content_lines(data_lines, max_chunk_chars);
            let is_split = split_parts.len() > 1;

            for (split_part, part) in split_parts.into_iter().enumerate() {
                if part.is_empty() {
                    continue;
                }
                let row_count = part.len();
                chunks.push(XlsxChunkRecord {
                    content: part.join("\n"),
                    content_type: CT_PAGE_AWARE.to_string(),
                    metadata: json!({
                        "sheet_name": sheet_name,
                        "sheet_index": sheet_index,
                        "has_print_area": false,
                        "print_area_ref": Option::<String>::None,
                        "start_row": start_row,
                        "end_row": end_row,
                        "start_col": start_col,
                        "end_col": end_col,
                        "row_count": row_count,
                        "col_count": col_count,
                        "header_row": &headers,
                        "region_index": split_part,
                        "chunk_index": chunk_index,
                        "is_split": is_split,
                        "split_part": if is_split { Some(split_part) } else { None::<usize> },
                    }),
                });
                chunk_index += 1;
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

