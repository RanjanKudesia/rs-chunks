use calamine::{Data, Reader};



fn escape_md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', " ")
}

fn render_sheet_markdown(sheet_name: &str, rows: &[Vec<String>], has_header: bool) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    let mut out = format!("## {}\n\n", sheet_name);
    out.push('|');
    for col in 0..max_cols {
        let cell = rows[0].get(col).map(String::as_str).unwrap_or("");
        out.push(' ');
        out.push_str(&escape_md_cell(cell));
        out.push_str(" |");
    }
    out.push('\n');

    if has_header && rows.len() > 1 {
        out.push('|');
        for _ in 0..max_cols {
            out.push_str(" --- |");
        }
        out.push('\n');
    }

    let data_start = if has_header { 1 } else { 0 };
    for row in rows.iter().skip(data_start) {
        out.push('|');
        for col in 0..max_cols {
            let cell = row.get(col).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&escape_md_cell(cell));
            out.push_str(" |");
        }
        out.push('\n');
    }

    out.trim_end().to_string()
}


pub(super) fn to_markdown(data: &[u8], ext: &str) -> Result<String, String> {
    use super::common::{
        detect_header_row, open_spreadsheet_from_bytes, read_worksheet_range, row_is_empty_public,
    };
    let mut workbook = open_spreadsheet_from_bytes(data, ext)?;
    let sheet_names = workbook.sheet_names().to_vec();
    let mut parts: Vec<String> = Vec::new();

    for sheet_name in &sheet_names {
        // A sheet calamine cannot read (chart sheets, XLM macro sheets) must not
        // take the whole workbook down with it — skip it and keep going.
        let Ok(range) = read_worksheet_range(&mut workbook, sheet_name)? else {
            continue;
        };
        let raw_rows: Vec<&[Data]> = range.rows().collect();
        if raw_rows.is_empty() || raw_rows.iter().all(|r| row_is_empty_public(r)) {
            continue;
        }
        let col_count = raw_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }
        let string_rows: Vec<Vec<String>> = raw_rows
            .iter()
            .map(|row| {
                (0..col_count)
                    .map(|i| row.get(i).map(super::common::cell_to_string).unwrap_or_default())
                    .collect()
            })
            .collect();
        let header_idx = detect_header_row(&raw_rows);
        let has_header = header_idx.map_or(raw_rows.len() > 1, |_| true);
        let ordered_rows: Vec<Vec<String>> = if let Some(h) = header_idx {
            let mut r = vec![string_rows[h].clone()];
            r.extend(string_rows[h + 1..].iter().cloned());
            r
        } else {
            string_rows
        };
        let rendered = render_sheet_markdown(sheet_name, &ordered_rows, has_header);
        if !rendered.is_empty() {
            parts.push(rendered);
        }
    }
    Ok(parts.join("\n\n---\n\n"))
}

pub(super) fn to_markdown_with_images(data: &[u8], ext: &str) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    use super::common::{
        detect_header_row, open_spreadsheet_from_bytes, read_worksheet_range, row_is_empty_public,
    };
    let mut workbook = open_spreadsheet_from_bytes(data, ext)?;
    let sheet_names = workbook.sheet_names().to_vec();

    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    let image_infos = super::images::collect_spreadsheet_images(data, ext, &sheet_names, &mut image_out);
    let mut sheet_image_map: std::collections::HashMap<usize, Vec<String>> = std::collections::HashMap::new();
    for info in image_infos {
        sheet_image_map.entry(info.sheet_index).or_default().push(info.hash_name);
    }

    let mut parts: Vec<String> = Vec::new();
    for (sheet_idx, sheet_name) in sheet_names.iter().enumerate() {
        // A sheet calamine cannot read (chart sheets, XLM macro sheets) must not
        // take the whole workbook down with it — skip it and keep going.
        let Ok(range) = read_worksheet_range(&mut workbook, sheet_name)? else {
            continue;
        };
        let sheet_images = sheet_image_map.get(&sheet_idx);
        let raw_rows: Vec<&[Data]> = range.rows().collect();
        let no_data = raw_rows.is_empty() || raw_rows.iter().all(|r| row_is_empty_public(r));
        if no_data {
            if let Some(hashes) = sheet_images {
                let mut section = format!("## {sheet_name}");
                for hash_name in hashes {
                    section.push_str(&format!("\n\n![]({hash_name})"));
                }
                parts.push(section);
            }
            continue;
        }
        let col_count = raw_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }
        let string_rows: Vec<Vec<String>> = raw_rows
            .iter()
            .map(|row| (0..col_count).map(|i| row.get(i).map(super::common::cell_to_string).unwrap_or_default()).collect())
            .collect();
        let header_idx = detect_header_row(&raw_rows);
        let has_header = header_idx.map_or(raw_rows.len() > 1, |_| true);
        let ordered_rows: Vec<Vec<String>> = if let Some(h) = header_idx {
            let mut r = vec![string_rows[h].clone()];
            r.extend(string_rows[h + 1..].iter().cloned());
            r
        } else {
            string_rows
        };
        let rendered = render_sheet_markdown(sheet_name, &ordered_rows, has_header);
        if rendered.is_empty() {
            continue;
        }
        let mut section = rendered;
        if let Some(hashes) = sheet_images {
            for hash_name in hashes {
                section.push_str(&format!("\n\n![]({hash_name})"));
            }
        }
        parts.push(section);
    }
    Ok((parts.join("\n\n---\n\n"), image_out))
}
