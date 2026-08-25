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
            // No data line to sniff (empty, or comments only). Nothing to
            // detect and nothing to chunk, so default to comma and let the
            // empty row set produce `[]` (TECH_DEBT T6). This used to raise
            // "CSV file is empty" — a *delimiter-detection* failure wearing an
            // emptiness message, which is why `.csv` and `.tsv` disagreed:
            // `.tsv` supplies `Some(b'\t')` and never reached this arm at all.
            match first_data_line_of(data, encoding)? {
                Some(line) => Ok(detect_delimiter(&line)),
                None => Ok(b','),
            }
        }
    }
}

pub(crate) fn normalize_headers(mut headers: Vec<String>, width: usize) -> Vec<String> {
    if headers.len() < width {
        headers.extend((headers.len()..width).map(|idx| format!("Column {}", idx + 1)));
    }
    headers
}

/// How many data rows the header heuristic looks at.
pub(crate) const HEADER_SNIFF_ROWS: usize = 20;

fn looks_numeric(value: &str) -> bool {
    let v = value.trim().trim_start_matches(['+', '-', '$', '£', '€']);
    let v = v.replace(',', "");
    !v.is_empty() && v.trim_end_matches('%').parse::<f64>().is_ok()
}

/// Decide whether the first record is a header row or just the first data row.
///
/// CSV has no way to say. Assuming "always a header" silently *deletes* the
/// first row of every headerless file, and with `include_headers=True` it then
/// stamps that row's values onto every other row as column labels.
///
/// The vote is per column, following the approach in Python's `csv.Sniffer`:
/// if a column's data is numeric and the first row's cell is not, that is a
/// header; if the column is textual, a first-row cell whose length differs from
/// the data's typical length is a header. Ties go to "not a header", because
/// treating data as a header loses content while the reverse only adds a
/// synthetic label.
pub(crate) fn first_row_is_header(first: &[String], data_rows: &[Vec<String>]) -> bool {
    // A file with no data rows is one row of data, not a lone header. Calling it
    // a header would return zero chunks — total content loss on a valid file.
    if data_rows.is_empty() || first.is_empty() {
        return false;
    }
    // Real headers label every column.
    if first.iter().any(|c| c.trim().is_empty()) {
        return false;
    }
    // A header cell that is itself a number is data.
    if first.iter().all(|c| looks_numeric(c)) {
        return false;
    }

    let sample: &[Vec<String>] = &data_rows[..data_rows.len().min(HEADER_SNIFF_ROWS)];
    let mut votes: i32 = 0;

    for (col, head) in first.iter().enumerate() {
        let values: Vec<&String> = sample.iter().filter_map(|r| r.get(col)).collect();
        if values.is_empty() {
            continue;
        }
        let numeric = values.iter().filter(|v| looks_numeric(v)).count();
        if numeric * 2 > values.len() {
            // Numeric column: a non-numeric label above it is a header.
            votes += if looks_numeric(head) { -1 } else { 1 };
        } else {
            // Textual column: compare against the typical data length.
            let mut lengths: Vec<usize> = values.iter().map(|v| v.trim().chars().count()).collect();
            lengths.sort_unstable();
            let median = lengths[lengths.len() / 2];
            let head_len = head.trim().chars().count();
            votes += if head_len == median { -1 } else { 1 };
        }
    }
    votes > 0
}

/// Positional labels for a file whose first row is data.
pub(crate) fn synthetic_headers(width: usize) -> Vec<String> {
    (1..=width).map(|i| format!("Column {i}")).collect()
}

pub(crate) fn is_empty_row(row: &[String]) -> bool {
    row.iter().all(|value| value.trim().is_empty())
}

/// Read the first record plus a short lookahead, then decide whether that first
/// record was a header or the first row of data.
///
/// Streaming cannot look at the whole file, but it must reach the *same*
/// conclusion the batch path does or the two disagree about the same input
/// (#25). Buffering `HEADER_SNIFF_ROWS` rows is enough for the heuristic and
/// bounded regardless of file size.
///
/// Returns the headers to use, whether they came from the file, and the rows
/// that must be processed before the rest of the reader. `None` means the file
/// held no records at all.
pub(crate) type HeaderDecision = (Vec<String>, bool, Vec<Vec<String>>);

pub(crate) fn read_header_with_lookahead<R: std::io::Read>(
    records: &mut csv::StringRecordsIter<'_, R>,
    skip_empty_rows: bool,
) -> Result<Option<HeaderDecision>, String> {
    let first = match records.next() {
        Some(Ok(record)) => record,
        Some(Err(err)) => return Err(format!("Failed to read CSV header: {err}")),
        None => return Ok(None),
    };
    let mut headers: Vec<String> = first.iter().map(|v| v.to_string()).collect();

    let mut lookahead: Vec<Vec<String>> = Vec::new();
    for record in records.by_ref() {
        let record = record.map_err(|err| format!("Failed to read CSV row: {err}"))?;
        let row: Vec<String> = record.iter().map(|v| v.to_string()).collect();
        if skip_empty_rows && is_empty_row(&row) {
            continue;
        }
        lookahead.push(row);
        if lookahead.len() >= HEADER_SNIFF_ROWS {
            break;
        }
    }

    let has_header = first_row_is_header(&headers, &lookahead);
    if !has_header {
        let width = headers
            .len()
            .max(lookahead.iter().map(Vec::len).max().unwrap_or(0));
        lookahead.insert(0, std::mem::take(&mut headers));
        headers = synthetic_headers(width);
    }
    Ok(Some((headers, has_header, lookahead)))
}

/// A parsed delimited file: the header cells, the data rows, the delimiter byte
/// actually used (which may have been sniffed), and whether the first record was
/// treated as a header.
pub(crate) type ParsedCsv = (Vec<String>, Vec<Vec<String>>, u8, bool);

pub(crate) fn parse_csv_to_rows(
    data: &[u8],
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<ParsedCsv, String> {
    let text = decode_to_utf8(data, encoding)?;

    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    //
    // This must precede delimiter detection. `.csv` passes `None` and the
    // auto-detect raised; `.tsv` passes `Some(b'\t')` and skipped the check, so
    // identical bytes gave an error for one extension and a chunk for the
    // other. Guarding on the decoded text settles both, and stops a
    // whitespace-only line being resurrected as a header row below.
    if text.trim().is_empty() {
        return Ok((Vec::new(), Vec::new(), delimiter.unwrap_or(b','), false));
    }

    let delimiter = delimiter_byte(delimiter, data, encoding)?;
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
        None => return Ok((Vec::new(), Vec::new(), delimiter, false)),
    };

    let mut headers: Vec<String> = header_record
        .iter()
        .map(|value| value.to_string())
        .collect();
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

    // CSV cannot say whether row 1 is a header. Assuming it always is deletes
    // the first row of every headerless file — and with include_headers=True,
    // stamps its values onto every other row as labels. (#26)
    // Decide from the same short lookahead streaming uses, and do NOT pad to a
    // global maximum — streaming cannot see the whole file, so any use of
    // whole-file knowledge here makes the two paths disagree on a ragged file
    // and breaks the "streaming output is identical to batch" guarantee (#25).
    // Widths are grown per emitted chunk instead, exactly as streaming does.
    let sniff = &data_rows[..data_rows.len().min(HEADER_SNIFF_ROWS)];
    let has_header = first_row_is_header(&headers, sniff);
    if !has_header {
        let width = headers
            .len()
            .max(sniff.iter().map(Vec::len).max().unwrap_or(0));
        data_rows.insert(0, std::mem::take(&mut headers));
        headers = synthetic_headers(width);
    }
    let _ = max_width;

    Ok((headers, data_rows, delimiter, has_header))
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

    let (headers, data_rows, delimiter, has_header) =
        parse_csv_to_rows(data, delimiter, encoding, skip_empty_rows)?;

    let mut chunks = Vec::new();
    let mut row_start = 1usize;
    let mut headers = headers;

    for (chunk_index, group) in data_rows.chunks(rows_per_chunk).enumerate() {
        let row_count = group.len();
        let row_end = row_start + row_count - 1;
        // Grow to the widest row seen SO FAR, not the widest in the file —
        // that is all streaming can know at this point. (#25)
        let widest = group.iter().map(Vec::len).max().unwrap_or(0);
        if widest > headers.len() {
            headers = normalize_headers(headers, widest);
        }
        chunks.push(CsvChunkRecord {
            content: build_content(&headers, group, include_headers),
            content_type: CT_ROW_GROUP.to_string(),
            metadata: json!({
                "row_start": row_start,
                "row_end": row_end,
                "row_count": row_count,
                "col_count": headers.len(),
                "header_row": headers,
                "has_header": has_header,
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
