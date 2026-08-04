//! CSV/TSV sliding-window chunk builder (pure logic).

use serde_json::json;

use super::chunker::{normalize_headers, parse_csv_to_rows};
use super::common::{serialize_row_kv, serialize_row_values, CsvChunkRecord, CT_ROW_WINDOW};

fn build_chunk_content(headers: &[String], rows: &[Vec<String>], include_headers: bool) -> String {
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

pub fn build_sliding_window_chunks(
    data: &[u8],
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<Vec<CsvChunkRecord>, String> {
    if window_size == 0 {
        return Err("window_size must be greater than 0".to_string());
    }
    if overlap >= window_size {
        return Err("overlap must be less than window_size".to_string());
    }

    let (headers, data_rows, delimiter, has_header) =
        parse_csv_to_rows(data, delimiter, encoding, skip_empty_rows)?;

    let mut chunks = Vec::new();
    let mut row_start = 1usize;
    let mut chunk_index = 0usize;
    let mut window_index = 0usize;
    let step = window_size - overlap;
    let mut cursor = 0usize;

    let mut headers = headers;
    while cursor < data_rows.len() {
        let end = (cursor + window_size).min(data_rows.len());
        let window = &data_rows[cursor..end];
        let row_end = row_start + window.len() - 1;
        // Widest row seen so far, matching what streaming can know. (#25)
        let widest = window.iter().map(Vec::len).max().unwrap_or(0);
        if widest > headers.len() {
            headers = normalize_headers(headers, widest);
        }
        chunks.push(CsvChunkRecord {
            content: build_chunk_content(&headers, window, include_headers),
            content_type: CT_ROW_WINDOW.to_string(),
            metadata: json!({
                "window_index": window_index,
                "window_size": window_size,
                "overlap": overlap,
                "row_start": row_start,
                "row_end": row_end,
                "actual_row_count": window.len(),
                "header_row": headers,
                "has_header": has_header,
                "col_count": headers.len(),
                "delimiter_detected": delimiter_str(delimiter),
                "encoding": encoding.to_ascii_lowercase(),
                "chunk_index": chunk_index,
            }),
        });
        row_start += step;
        chunk_index += 1;
        window_index += 1;
        cursor += step;
    }

    Ok(chunks)
}
