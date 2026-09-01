//! CSV/TSV → Markdown pipe table (native).

use super::chunker::{first_row_is_header, synthetic_headers};
use csv::{ReaderBuilder, Trim};

use super::chunker::{is_empty_row, normalize_headers};
use super::common::detect_delimiter;
use crate::error::{ChunkError, Result};

pub fn csv_to_markdown(file_path: &str, delimiter: Option<u8>, encoding: &str) -> Result<String> {
    let path = std::path::Path::new(file_path);
    if !path.exists() {
        return Err(ChunkError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("CSV file not found: {file_path}"),
        )));
    }
    let data = std::fs::read(file_path).map_err(ChunkError::Io)?;
    csv_to_markdown_from_bytes(&data, delimiter, encoding)
}

/// No-filesystem CSV → Markdown table (wasm/browser).
pub fn csv_to_markdown_from_bytes(
    data: &[u8],
    delimiter: Option<u8>,
    encoding: &str,
) -> Result<String> {
    use super::common::{decode_to_utf8, first_data_line_of};
    let delim_byte = match delimiter {
        Some(b) => b,
        None => {
            let first_line = first_data_line_of(data, encoding).map_err(ChunkError::Parse)?;
            match first_line {
                Some(line) => detect_delimiter(&line),
                None => return Ok(String::new()),
            }
        }
    };

    let text = decode_to_utf8(data, encoding).map_err(ChunkError::Parse)?;

    let mut reader = ReaderBuilder::new()
        .delimiter(delim_byte)
        .trim(Trim::None)
        .flexible(true)
        .has_headers(false)
        // No `.comment(...)`: RFC 4180 defines no comment convention, and neither
        // does the `text/tab-separated-values` registration. `#` is ordinary
        // TEXTDATA. Treating a leading `#` as a comment silently DELETED whole
        // records — measured, a row `#4,legacy sku,archived` vanished with no
        // error and no metadata trace, while the same character mid-field
        // survived. Any file whose first column holds issue numbers, SKUs, hex
        // colours or hashtags lost exactly those rows. A `#` preamble now
        // arrives as data, which is the honest reading: it IS in the file.
        .from_reader(text.as_bytes());

    let mut records = reader.records();

    let header_record = match records.next() {
        Some(Ok(r)) => r,
        Some(Err(e)) => return Err(ChunkError::Parse(format!("Failed to read CSV header: {e}"))),
        None => return Ok(String::new()),
    };

    let mut headers: Vec<String> = header_record.iter().map(|v| v.to_string()).collect();
    let mut data_rows: Vec<Vec<String>> = Vec::new();
    let mut max_width = headers.len();

    for result in records {
        let record =
            result.map_err(|e| ChunkError::Parse(format!("Failed to read CSV row: {e}")))?;
        let row: Vec<String> = record.iter().map(|v| v.to_string()).collect();
        if is_empty_row(&row) {
            continue;
        }
        // One pathological row must not widen every other row: the padding
        // loop below costs `rows x max_width` empty Strings, so a single
        // 1M-field row in a 1M-row file is 10^12 allocations. Excel's column
        // limit is a generous ceiling for a markdown table.
        max_width = max_width
            .max(row.len())
            .min(crate::formats::xlsx::common::MAX_SHEET_COLS);
        data_rows.push(row);
    }

    // A headerless file must not lose its first row to the header slot. (#26)
    if !first_row_is_header(&headers, &data_rows) {
        max_width = max_width.max(headers.len());
        data_rows.insert(0, std::mem::take(&mut headers));
        headers = synthetic_headers(max_width);
    }
    headers = normalize_headers(headers, max_width);

    for row in &mut data_rows {
        while row.len() < max_width {
            row.push(String::new());
        }
    }

    if max_width == 0 {
        return Ok(String::new());
    }

    let escape = |s: &str| s.replace('|', "\\|");

    let header_line = format!(
        "| {} |",
        headers
            .iter()
            .map(|h| escape(h))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let sep_line = format!("| {} |", vec!["---"; max_width].join(" | "));

    let mut lines: Vec<String> = vec![header_line, sep_line];
    for row in &data_rows {
        let cell_line = format!(
            "| {} |",
            row.iter()
                .map(|c| escape(c))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        lines.push(cell_line);
    }

    Ok(lines.join("\n"))
}
