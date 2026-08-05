use std::io::Read;

use calamine::{Data, Reader};
use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader as XmlReader;
use serde_json::json;
use zip::ZipArchive;

use super::common::{
    cell_to_string, detect_contiguous_regions, detect_header_row, parse_range_ref,
    row_is_empty_public, serialize_row_kv, serialize_row_values_public, XlsxChunkRecord, CT_TABLE,
};

#[derive(Debug, Clone)]
pub struct TableInfo {
    pub name: Option<String>,
    pub is_named_table: bool,
    pub start_row: usize,
    pub end_row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub headers: Vec<String>,
}

fn read_zip_entry(archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> Result<Option<Vec<u8>>, String> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read '{name}': {e}"))?;
            Ok(Some(buf))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("Failed to open '{name}' in xlsx archive: {e}")),
    }
}

fn local_name(name: QName<'_>) -> Vec<u8> {
    let bytes = name.as_ref();
    let idx = bytes
        .iter()
        .rposition(|b| *b == b':')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[idx..].to_vec()
}

fn attr_value(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    String::from_utf8_lossy(attr.value.as_ref()).into_owned()
}

fn resolve_target(base_dir: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').collect();
    for segment in target.split('/') {
        match segment {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn parse_table_relationship_targets(rels_xml: &[u8]) -> Result<Vec<String>, String> {
    let mut reader = XmlReader::from_reader(rels_xml);
    let mut buf = Vec::new();
    let mut targets = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse worksheet relationships XML: {e}")),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"Relationship" {
                    let mut rel_type = String::new();
                    let mut target = String::new();
                    for attr in e.attributes().flatten() {
                        let key = local_name(QName(attr.key.as_ref()));
                        if key.as_slice() == b"Type" {
                            rel_type = attr_value(&attr);
                        } else if key.as_slice() == b"Target" {
                            target = attr_value(&attr);
                        }
                    }
                    if rel_type.ends_with("/table") && !target.is_empty() {
                        targets.push(target);
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(targets)
}

fn parse_table_definition(table_xml: &[u8]) -> Result<Option<TableInfo>, String> {
    let mut reader = XmlReader::from_reader(table_xml);
    let mut buf = Vec::new();

    let mut table_name: Option<String> = None;
    let mut display_name: Option<String> = None;
    let mut range_ref: Option<String> = None;
    let mut headers: Vec<String> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse table XML: {e}")),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let lname = local_name(e.name());
                if lname.as_slice() == b"table" {
                    for attr in e.attributes().flatten() {
                        let key = local_name(QName(attr.key.as_ref()));
                        let value = attr_value(&attr);
                        if key.as_slice() == b"name" {
                            table_name = Some(value);
                        } else if key.as_slice() == b"displayName" {
                            display_name = Some(value);
                        } else if key.as_slice() == b"ref" {
                            range_ref = Some(value);
                        }
                    }
                } else if lname.as_slice() == b"tableColumn" {
                    for attr in e.attributes().flatten() {
                        let key = local_name(QName(attr.key.as_ref()));
                        if key.as_slice() == b"name" {
                            headers.push(attr_value(&attr));
                        }
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let Some(range_ref) = range_ref else {
        return Ok(None);
    };
    let Some((start_row, start_col, end_row, end_col)) = parse_range_ref(&range_ref) else {
        return Ok(None);
    };

    Ok(Some(TableInfo {
        name: table_name.or(display_name),
        is_named_table: true,
        start_row,
        end_row,
        start_col,
        end_col,
        headers,
    }))
}

fn get_row_values(row: &[Data], start_col: usize, end_col: usize) -> Vec<Data> {
    (start_col..=end_col)
        .map(|col| row.get(col).cloned().unwrap_or(Data::Empty))
        .collect()
}

fn serialize_table_row(values: &[Data], headers: &[String], include_headers: bool) -> String {
    if include_headers {
        serialize_row_kv(headers, values)
    } else {
        serialize_row_values_public(values, headers.len())
    }
}

fn split_lines_by_max_chars(
    lines: &[(usize, String)],
    max_chunk_chars: usize,
) -> Vec<Vec<(usize, String)>> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut parts: Vec<Vec<(usize, String)>> = Vec::new();
    let mut current: Vec<(usize, String)> = Vec::new();
    let mut current_len = 0usize;

    for (row_index, line) in lines {
        let line_len = line.len();
        let sep_len = if current.is_empty() { 0 } else { 1 };
        let proposed = current_len + sep_len + line_len;
        if !current.is_empty() && proposed > max_chunk_chars {
            parts.push(current);
            current = vec![(*row_index, line.clone())];
            current_len = line_len;
        } else {
            current.push((*row_index, line.clone()));
            current_len = proposed;
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn build_headers_from_cells(row_cells: &[Data], col_count: usize) -> Vec<String> {
    (0..col_count)
        .map(|idx| {
            let value = row_cells.get(idx).map(cell_to_string).unwrap_or_default();
            if value.trim().is_empty() {
                format!("Column {}", idx + 1)
            } else {
                value
            }
        })
        .collect()
}

fn read_named_tables_for_sheet(
    archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>,
    sheet_index_1based: usize,
) -> Result<Vec<TableInfo>, String> {
    let rels_path = format!("xl/worksheets/_rels/sheet{}.xml.rels", sheet_index_1based);
    let Some(rels_xml) = read_zip_entry(archive, &rels_path)? else {
        return Ok(Vec::new());
    };

    let targets = parse_table_relationship_targets(&rels_xml)?;
    let mut tables = Vec::new();
    for target in targets {
        let full_path = resolve_target("xl/worksheets", &target);
        let Some(table_xml) = read_zip_entry(archive, &full_path)? else {
            continue;
        };
        if let Some(info) = parse_table_definition(&table_xml)? {
            tables.push(info);
        }
    }
    Ok(tables)
}

fn build_heuristic_tables(rows: &[&[Data]], base_row: usize) -> Vec<TableInfo> {
    detect_contiguous_regions(rows, base_row)
        .into_iter()
        .map(|region| TableInfo {
            name: None,
            is_named_table: false,
            start_row: region.start_row,
            end_row: region.end_row,
            start_col: region.start_col,
            end_col: region.end_col,
            headers: Vec::new(),
        })
        .collect()
}

pub fn build_table_chunks(
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

    // Try to open as a ZIP for named-table discovery. For XLS (BIFF binary), this
    // silently yields None and we fall back to heuristic region detection per sheet.
    let mut maybe_archive: Option<ZipArchive<std::io::Cursor<Vec<u8>>>> =
        ZipArchive::new(std::io::Cursor::new(data.to_vec())).ok();

    let mut chunks = Vec::new();
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

        let mut tables = if let Some(ref mut archive) = maybe_archive {
            read_named_tables_for_sheet(archive, sheet_index + 1)?
        } else {
            Vec::new()
        };
        if tables.is_empty() {
            tables = build_heuristic_tables(&rows, base_row_index);
        }

        let mut chunk_index = 0usize;
        for mut table in tables {
            if table.start_col > table.end_col || table.start_row > table.end_row {
                continue;
            }

            let col_count = table.end_col - table.start_col + 1;

            let mut header_row_abs = table.start_row;
            if table.is_named_table {
                if table.headers.is_empty() {
                    let header_local = table.start_row.saturating_sub(base_row_index);
                    if let Some(header_row) = rows.get(header_local) {
                        let header_values =
                            get_row_values(header_row, table.start_col, table.end_col);
                        table.headers = build_headers_from_cells(&header_values, col_count);
                    } else {
                        table.headers = (0..col_count)
                            .map(|i| format!("Column {}", i + 1))
                            .collect();
                    }
                }
            } else {
                let local_start = table.start_row.saturating_sub(base_row_index);
                let local_end = table.end_row.saturating_sub(base_row_index);
                if local_start >= rows.len() || local_end >= rows.len() || local_start > local_end {
                    continue;
                }

                let region_rows_owned: Vec<Vec<Data>> = (local_start..=local_end)
                    .map(|idx| get_row_values(rows[idx], table.start_col, table.end_col))
                    .collect();
                let region_row_refs: Vec<&[Data]> =
                    region_rows_owned.iter().map(|r| r.as_slice()).collect();
                let header_rel = detect_header_row(&region_row_refs);
                if let Some(rel) = header_rel {
                    header_row_abs = table.start_row + rel;
                }

                let header_cells = header_rel
                    .and_then(|rel| region_rows_owned.get(rel))
                    .cloned()
                    .unwrap_or_else(|| vec![Data::Empty; col_count]);
                table.headers = build_headers_from_cells(&header_cells, col_count);
            }

            if table.headers.len() < col_count {
                table
                    .headers
                    .extend((table.headers.len()..col_count).map(|i| format!("Column {}", i + 1)));
            }
            if table.headers.len() > col_count {
                table.headers.truncate(col_count);
            }

            let data_start_row = if table.is_named_table {
                table.start_row + 1
            } else {
                header_row_abs + 1
            };

            let mut row_lines: Vec<(usize, String)> = Vec::new();
            // `data_start_row > end_row` means the header consumed the region;
            // fall through to the fallback below rather than dropping it here.
            if data_start_row <= table.end_row {
                for abs_row in data_start_row..=table.end_row {
                    let local_row = abs_row.saturating_sub(base_row_index);
                    let Some(row) = rows.get(local_row) else {
                        continue;
                    };
                    let values = get_row_values(row, table.start_col, table.end_col);
                    if skip_empty_rows && row_is_empty_public(&values) {
                        continue;
                    }
                    let line = serialize_table_row(&values, &table.headers, include_headers);
                    row_lines.push((abs_row, line));
                }
            }

            // Header fallback, same rule the row-consuming modes apply via
            // `data_start_with_header_fallback`: a region whose only content was
            // taken as its header still holds that content (TECH_DEBT #80).
            if row_lines.is_empty() {
                let local_header = header_row_abs.saturating_sub(base_row_index);
                if let Some(header_row) = rows.get(local_header) {
                    let values = get_row_values(header_row, table.start_col, table.end_col);
                    if !row_is_empty_public(&values) {
                        let line = serialize_table_row(&values, &table.headers, include_headers);
                        row_lines.push((header_row_abs, line));
                    }
                }
            }

            if row_lines.is_empty() {
                continue;
            }

            let split_parts = split_lines_by_max_chars(&row_lines, max_chunk_chars);
            let is_split = split_parts.len() > 1;
            for (split_part, part_rows) in split_parts.into_iter().enumerate() {
                if part_rows.is_empty() {
                    continue;
                }
                let content = part_rows
                    .iter()
                    .map(|(_, line)| line.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let row_count = part_rows.len();
                let part_start_row = part_rows
                    .first()
                    .map(|(r, _)| *r)
                    .unwrap_or(table.start_row);
                let part_end_row = part_rows.last().map(|(r, _)| *r).unwrap_or(table.end_row);

                chunks.push(XlsxChunkRecord {
                    content,
                    content_type: CT_TABLE.to_string(),
                    metadata: json!({
                        "sheet_name": sheet_name,
                        "sheet_index": sheet_index,
                        "table_name": table.name,
                        "is_named_table": table.is_named_table,
                        "header_row": table.headers,
                        "start_row": part_start_row,
                        "end_row": part_end_row,
                        "start_col": table.start_col,
                        "end_col": table.end_col,
                        "row_count": row_count,
                        "col_count": col_count,
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

