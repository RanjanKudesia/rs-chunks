//! CSV/TSV row + page-aware chunk builders (pure logic, ported verbatim from the
//! Python engine minus the PyO3 boundary).

use csv::{ReaderBuilder, Trim};
use serde_json::json;

use super::common::{
    decode_to_utf8, detect_delimiter, first_data_line_of, serialize_row_kv, serialize_row_values,
    CsvChunkRecord, CT_ROW_GROUP,
};

pub(crate) fn delimiter_byte(
    delimiter: Option<u8>,
    data: &[u8],
    encoding: &str,
) -> Result<u8, String> {
    match delimiter {
        Some(byte) => Ok(byte),
        None => {
            let first_line = first_data_line_of(data, encoding)?
                .ok_or_else(|| "CSV file is empty".to_string())?;
            Ok(detect_delimiter(&first_line))
        }
    }
}

pub(crate) fn normalize_headers(mut headers: Vec<String>, width: usize) -> Vec<String> {
    if headers.len() < width {
        headers.extend((headers.len()..width).map(|idx| format!("Column {}", idx + 1)));
    }
    headers
}

pub(crate) fn is_empty_row(row: &[String]) -> bool {
    row.iter().all(|value| value.trim().is_empty())
}

pub(crate) fn parse_csv_to_rows(
    data: &[u8],
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<(Vec<String>, Vec<Vec<String>>, u8), String> {
    let delimiter = delimiter_byte(delimiter, data, encoding)?;
    let text = decode_to_utf8(data, encoding)?;
    let mut reader = ReaderBuilder::new()
        .delimiter(delimiter)
        .trim(Trim::None)
        .flexible(true)
        .has_headers(false)
        .comment(Some(b'#'))
        .from_reader(text.as_bytes());

    let mut records = reader.records();
    let header_record = match records.next() {
        Some(Ok(record)) => record,
        Some(Err(err)) => return Err(format!("Failed to read CSV header: {err}")),
        None => return Ok((Vec::new(), Vec::new(), delimiter)),
    };

    let mut headers: Vec<String> = header_record.iter().map(|value| value.to_string()).collect();
    let mut data_rows: Vec<Vec<String>> = Vec::new();
    let mut max_width = headers.len();

    for record in records {
        let record = record.map_err(|err| format!("Failed to read CSV row: {err}"))?;
        let row: Vec<String> = record.iter().map(|value| value.to_string()).collect();
        if skip_empty_rows && is_empty_row(&row) {
            continue;
        }
        max_width = max_width.max(row.len());
        data_rows.push(row);
    }

    headers = normalize_headers(headers, max_width);
    for row in &mut data_rows {
        if row.len() < max_width {
            row.extend(std::iter::repeat_with(String::new).take(max_width - row.len()));
        }
    }

    Ok((headers, data_rows, delimiter))
}

fn build_content(headers: &[String], rows: &[Vec<String>], include_headers: bool) -> String {
    rows.iter()
        .map(|row| {
            if include_headers {
                serialize_row_kv(headers, row)
            } else {
                serialize_row_values(row)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn delimiter_str(delimiter: u8) -> String {
    char::from(delimiter).to_string()
}

pub fn build_row_chunks(
    data: &[u8],
    rows_per_chunk: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<Vec<CsvChunkRecord>, String> {
    if rows_per_chunk == 0 {
        return Err("rows_per_chunk must be greater than 0".to_string());
    }

    let (headers, data_rows, delimiter) =
        parse_csv_to_rows(data, delimiter, encoding, skip_empty_rows)?;

    let mut chunks = Vec::new();
    let mut row_start = 1usize;

    for (chunk_index, group) in data_rows.chunks(rows_per_chunk).enumerate() {
        let row_count = group.len();
        let row_end = row_start + row_count - 1;
        chunks.push(CsvChunkRecord {
            content: build_content(&headers, group, include_headers),
            content_type: CT_ROW_GROUP.to_string(),
            metadata: json!({
                "row_start": row_start,
                "row_end": row_end,
                "row_count": row_count,
                "col_count": headers.len(),
                "header_row": headers,
                "delimiter_detected": delimiter_str(delimiter),
                "encoding": encoding.to_ascii_lowercase(),
                "chunk_index": chunk_index,
            }),
        });
        row_start = row_end + 1;
    }

    Ok(chunks)
}

pub fn build_page_aware_chunks(
    data: &[u8],
    rows_per_page: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<Vec<CsvChunkRecord>, String> {
    build_row_chunks(
        data,
        rows_per_page,
        include_headers,
        delimiter,
        encoding,
        skip_empty_rows,
    )
}
