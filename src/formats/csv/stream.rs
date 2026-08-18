//! Streaming CSV/TSV chunker. A worker thread parses + decodes incrementally and
//! sends chunks over an `mpsc` channel; the public `CsvStreamIterator` wraps the
//! receiver as a native `Iterator`. Ported from the Python engine's streaming
//! path minus the `#[pyclass]` boundary.

use super::chunker::read_header_with_lookahead;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::mpsc;
use std::thread;

use csv::ReaderBuilder;
use serde_json::json;

use super::common::{
    detect_delimiter, serialize_row_kv, serialize_row_values, CT_ROW_GROUP, CT_ROW_WINDOW,
};
use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};

struct RawCsvChunk {
    content: String,
    content_type: &'static str,
    metadata: serde_json::Value,
}

/// Native streaming iterator over CSV/TSV chunks.
pub struct CsvStreamIterator {
    receiver: mpsc::Receiver<std::result::Result<RawCsvChunk, String>>,
    _thread: thread::JoinHandle<()>,
    done: bool,
}

impl Iterator for CsvStreamIterator {
    type Item = Result<Chunk>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.receiver.recv() {
            Ok(Ok(raw)) => Some(Ok(Chunk::new(raw.content, raw.content_type, raw.metadata))),
            Ok(Err(err)) => {
                self.done = true;
                Some(Err(ChunkError::Parse(err)))
            }
            Err(_) => {
                self.done = true;
                None
            }
        }
    }
}

enum CsvEncoding {
    Utf8,
    Utf8Bom,
    Latin1,
    Windows1252,
}

struct DecodingReader<R: Read> {
    inner: R,
    encoding: CsvEncoding,
    utf8_carry: Vec<u8>,
    pending: Vec<u8>,
    pending_pos: usize,
}

impl<R: Read> DecodingReader<R> {
    fn new(inner: R, encoding: &str) -> std::result::Result<Self, String> {
        let encoding = match encoding.to_ascii_lowercase().as_str() {
            "utf-8" => CsvEncoding::Utf8,
            "utf-8-bom" => CsvEncoding::Utf8Bom,
            "latin-1" => CsvEncoding::Latin1,
            "windows-1252" => CsvEncoding::Windows1252,
            other => return Err(format!("Unsupported encoding: {other}")),
        };
        Ok(Self {
            inner,
            encoding,
            utf8_carry: Vec::new(),
            pending: Vec::new(),
            pending_pos: 0,
        })
    }

    fn decode_chunk(&self, bytes: &[u8]) -> std::result::Result<String, String> {
        match self.encoding {
            CsvEncoding::Utf8 => String::from_utf8(bytes.to_vec())
                .map_err(|e| format!("Failed to decode CSV bytes as utf-8: {e}")),
            CsvEncoding::Utf8Bom => {
                let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                    &bytes[3..]
                } else {
                    bytes
                };
                String::from_utf8(slice.to_vec())
                    .map_err(|e| format!("Failed to decode CSV bytes as utf-8-bom: {e}"))
            }
            CsvEncoding::Latin1 => Ok(bytes.iter().map(|&b| char::from(b)).collect()),
            CsvEncoding::Windows1252 => Ok(bytes.iter().map(|&b| windows_1252_char(b)).collect()),
        }
    }

    fn fill_pending(&mut self) -> io::Result<bool> {
        if self.pending_pos < self.pending.len() {
            return Ok(true);
        }

        self.pending.clear();
        self.pending_pos = 0;

        let mut raw_buf = [0u8; 8192];
        let read = self.inner.read(&mut raw_buf)?;
        if read == 0 {
            match self.encoding {
                CsvEncoding::Utf8 | CsvEncoding::Utf8Bom => {
                    if self.utf8_carry.is_empty() {
                        return Ok(false);
                    }
                    let decoded = String::from_utf8(std::mem::take(&mut self.utf8_carry))
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    self.pending = decoded.into_bytes();
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }

        let raw = &raw_buf[..read];
        let decoded = match self.encoding {
            CsvEncoding::Utf8 => {
                self.utf8_carry.extend_from_slice(raw);
                match String::from_utf8(self.utf8_carry.clone()) {
                    Ok(text) => {
                        self.utf8_carry.clear();
                        text.into_bytes()
                    }
                    Err(err) => {
                        let valid_up_to = err.utf8_error().valid_up_to();
                        if err.utf8_error().error_len().is_none() {
                            let valid = self.utf8_carry[..valid_up_to].to_vec();
                            self.utf8_carry = self.utf8_carry[valid_up_to..].to_vec();
                            valid
                        } else {
                            return Err(io::Error::new(io::ErrorKind::InvalidData, err));
                        }
                    }
                }
            }
            CsvEncoding::Utf8Bom => self
                .decode_chunk(raw)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                .into_bytes(),
            CsvEncoding::Latin1 | CsvEncoding::Windows1252 => self
                .decode_chunk(raw)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                .into_bytes(),
        };

        self.pending = decoded;
        Ok(true)
    }
}

impl<R: Read> Read for DecodingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if !self.fill_pending()? {
            return Ok(0);
        }
        let available = self.pending.len() - self.pending_pos;
        let count = available.min(out.len());
        out[..count].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + count]);
        self.pending_pos += count;
        Ok(count)
    }
}

fn normalize_headers(mut headers: Vec<String>, width: usize) -> Vec<String> {
    if headers.len() < width {
        headers.extend((headers.len()..width).map(|idx| format!("Column {}", idx + 1)));
    }
    headers
}

fn is_empty_row(row: &[String]) -> bool {
    row.iter().all(|value| value.trim().is_empty())
}

fn decode_line_bytes(bytes: &[u8], encoding: &str) -> std::result::Result<String, String> {
    match encoding.to_ascii_lowercase().as_str() {
        "utf-8" => String::from_utf8(bytes.to_vec())
            .map_err(|e| format!("Failed to decode CSV bytes as utf-8: {e}")),
        "utf-8-bom" => {
            let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                &bytes[3..]
            } else {
                bytes
            };
            String::from_utf8(slice.to_vec())
                .map_err(|e| format!("Failed to decode CSV bytes as utf-8-bom: {e}"))
        }
        "latin-1" => Ok(bytes.iter().map(|&b| char::from(b)).collect()),
        "windows-1252" => Ok(bytes.iter().map(|&b| windows_1252_char(b)).collect()),
        other => Err(format!("Unsupported encoding: {other}")),
    }
}

fn windows_1252_char(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}',
        0x81 => '\u{0081}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8D => '\u{008D}',
        0x8E => '\u{017D}',
        0x8F => '\u{008F}',
        0x90 => '\u{0090}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9D => '\u{009D}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => char::from(byte),
    }
}

fn detect_delimiter_from_file(file_path: &str, encoding: &str) -> std::result::Result<u8, String> {
    let file = File::open(file_path).map_err(|e| format!("Failed to open file: {e}"))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("Failed to read CSV line: {e}"))?;
        if read == 0 {
            return Err("CSV file is empty".to_string());
        }
        let line = decode_line_bytes(&buf, encoding)?;
        if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            return Ok(detect_delimiter(&line));
        }
    }
}

fn open_csv_reader(
    file_path: &str,
    delimiter: Option<u8>,
    encoding: &str,
) -> std::result::Result<(csv::Reader<Box<dyn Read + Send>>, u8), String> {
    let delimiter = match delimiter {
        Some(byte) => byte,
        None => detect_delimiter_from_file(file_path, encoding)?,
    };
    let file = File::open(file_path).map_err(|e| format!("Failed to open file: {e}"))?;
    let reader = DecodingReader::new(file, encoding)?;
    let boxed: Box<dyn Read + Send> = Box::new(reader);
    let reader = ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .has_headers(false)
        .comment(Some(b'#'))
        .from_reader(boxed);
    Ok((reader, delimiter))
}

// Eight independent values with no natural grouping; bundling them into a
// struct would add a type whose only purpose is to satisfy the lint.
#[allow(clippy::too_many_arguments)]
fn build_row_group_chunk(
    headers: &[String],
    has_header: bool,
    rows: &[Vec<String>],
    include_headers: bool,
    delimiter: u8,
    encoding: &str,
    chunk_index: usize,
    row_start: usize,
) -> RawCsvChunk {
    let row_end = row_start + rows.len() - 1;
    let content = rows
        .iter()
        .map(|row| {
            if include_headers {
                serialize_row_kv(headers, row)
            } else {
                serialize_row_values(row)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    RawCsvChunk {
        content,
        content_type: CT_ROW_GROUP,
        metadata: json!({
            "row_start": row_start,
            "row_end": row_end,
            "row_count": rows.len(),
            "col_count": headers.len(),
            "header_row": headers,
            "has_header": has_header,
            "delimiter_detected": char::from(delimiter).to_string(),
            "encoding": encoding.to_ascii_lowercase(),
            "chunk_index": chunk_index,
        }),
    }
}

fn build_row_streaming(
    file_path: &str,
    rows_per_chunk: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
    sender: mpsc::Sender<std::result::Result<RawCsvChunk, String>>,
) -> std::result::Result<(), String> {
    if rows_per_chunk == 0 {
        return Err("rows_per_chunk must be greater than 0".to_string());
    }

    let (mut reader, used_delimiter) = open_csv_reader(file_path, delimiter, encoding)?;
    let mut records = reader.records();
    let Some((mut headers, has_header, prelude)) =
        read_header_with_lookahead(&mut records, skip_empty_rows)?
    else {
        return Err("CSV file is empty".to_string());
    };
    let mut buffer: Vec<Vec<String>> = Vec::new();
    let mut row_start = 1usize;
    let mut chunk_index = 0usize;

    // Process the lookahead rows before the rest of the reader, so deciding
    // header-vs-data costs nothing in output order.
    let rest = records.map(|r| {
        r.map(|rec| rec.iter().map(|v| v.to_string()).collect::<Vec<String>>())
            .map_err(|err| format!("Failed to read CSV row: {err}"))
    });
    for row in prelude.into_iter().map(Ok).chain(rest) {
        let row: Vec<String> = row?;
        if skip_empty_rows && is_empty_row(&row) {
            continue;
        }
        if row.len() > headers.len() {
            headers = normalize_headers(headers, row.len());
        }
        buffer.push(row);
        if buffer.len() == rows_per_chunk {
            let chunk = build_row_group_chunk(
                &headers,
                has_header,
                &buffer,
                include_headers,
                used_delimiter,
                encoding,
                chunk_index,
                row_start,
            );
            sender.send(Ok(chunk)).map_err(|err| err.to_string())?;
            row_start += buffer.len();
            buffer.clear();
            chunk_index += 1;
        }
    }

    if !buffer.is_empty() {
        let chunk = build_row_group_chunk(
            &headers,
            has_header,
            &buffer,
            include_headers,
            used_delimiter,
            encoding,
            chunk_index,
            row_start,
        );
        sender.send(Ok(chunk)).map_err(|err| err.to_string())?;
    }

    Ok(())
}

// Mirrors the public `csv::chunk` parameter list one-for-one; regrouping here
// would only move the argument count to the call site.
#[allow(clippy::too_many_arguments)]
fn build_sliding_window_streaming(
    file_path: &str,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
    sender: mpsc::Sender<std::result::Result<RawCsvChunk, String>>,
) -> std::result::Result<(), String> {
    if window_size == 0 {
        return Err("window_size must be greater than 0".to_string());
    }
    if overlap >= window_size {
        return Err("overlap must be less than window_size".to_string());
    }

    let (mut reader, used_delimiter) = open_csv_reader(file_path, delimiter, encoding)?;
    let mut records = reader.records();
    let Some((mut headers, has_header, prelude)) =
        read_header_with_lookahead(&mut records, skip_empty_rows)?
    else {
        return Err("CSV file is empty".to_string());
    };
    let mut buffer: VecDeque<Vec<String>> = VecDeque::new();
    let mut row_start = 1usize;
    let mut chunk_index = 0usize;
    let mut window_index = 0usize;
    let step = window_size - overlap;

    // Process the lookahead rows before the rest of the reader, so deciding
    // header-vs-data costs nothing in output order.
    let rest = records.map(|r| {
        r.map(|rec| rec.iter().map(|v| v.to_string()).collect::<Vec<String>>())
            .map_err(|err| format!("Failed to read CSV row: {err}"))
    });
    for row in prelude.into_iter().map(Ok).chain(rest) {
        let row: Vec<String> = row?;
        if skip_empty_rows && is_empty_row(&row) {
            continue;
        }
        if row.len() > headers.len() {
            headers = normalize_headers(headers, row.len());
        }
        buffer.push_back(row);
        if buffer.len() == window_size {
            let rows: Vec<Vec<String>> = buffer.iter().cloned().collect();
            let row_end = row_start + rows.len() - 1;
            let content = rows
                .iter()
                .map(|row| {
                    if include_headers {
                        serialize_row_kv(&headers, row)
                    } else {
                        serialize_row_values(row)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            sender
                .send(Ok(RawCsvChunk {
                    content,
                    content_type: CT_ROW_WINDOW,
                    metadata: json!({
                                "window_index": window_index,
                                "window_size": window_size,
                                "overlap": overlap,
                                "row_start": row_start,
                                "row_end": row_end,
                                "actual_row_count": rows.len(),
                                "header_row": headers,
                    "has_header": has_header,
                                "col_count": headers.len(),
                                "delimiter_detected": char::from(used_delimiter).to_string(),
                                "encoding": encoding.to_ascii_lowercase(),
                                "chunk_index": chunk_index,
                            }),
                }))
                .map_err(|err| err.to_string())?;

            row_start += step;
            chunk_index += 1;
            window_index += 1;
            for _ in 0..step {
                buffer.pop_front();
            }
        }
    }

    if !buffer.is_empty() {
        let rows: Vec<Vec<String>> = buffer.iter().cloned().collect();
        let row_end = row_start + rows.len() - 1;
        let content = rows
            .iter()
            .map(|row| {
                if include_headers {
                    serialize_row_kv(&headers, row)
                } else {
                    serialize_row_values(row)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        sender
            .send(Ok(RawCsvChunk {
                content,
                content_type: CT_ROW_WINDOW,
                metadata: json!({
                        "window_index": window_index,
                        "window_size": window_size,
                        "overlap": overlap,
                        "row_start": row_start,
                        "row_end": row_end,
                        "actual_row_count": rows.len(),
                        "header_row": headers,
                "has_header": has_header,
                        "col_count": headers.len(),
                        "delimiter_detected": char::from(used_delimiter).to_string(),
                        "encoding": encoding.to_ascii_lowercase(),
                        "chunk_index": chunk_index,
                    }),
            }))
            .map_err(|err| err.to_string())?;
    }

    Ok(())
}

// Owns one thread's copy of the public parameter list — see the note above.
#[allow(clippy::too_many_arguments)]
fn run_worker(
    file_path: String,
    mode: String,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: String,
    skip_empty_rows: bool,
    sender: mpsc::Sender<std::result::Result<RawCsvChunk, String>>,
) -> std::result::Result<(), String> {
    match mode.as_str() {
        "row" | "default" | "page_aware" => build_row_streaming(
            &file_path,
            rows_per_chunk,
            include_headers,
            delimiter,
            &encoding,
            skip_empty_rows,
            sender,
        ),
        "sliding_window" => build_sliding_window_streaming(
            &file_path,
            window_size,
            overlap,
            include_headers,
            delimiter,
            &encoding,
            skip_empty_rows,
            sender,
        ),
        _ => Err(
            "mode must be 'row', 'default', 'sliding_window', or 'page_aware' for CSV streaming"
                .to_string(),
        ),
    }
}

/// Create a streaming iterator over CSV/TSV chunks.
#[allow(clippy::too_many_arguments)]
pub fn stream(
    file_path: &str,
    mode: &str,
    rows_per_chunk: usize,
    window_size: usize,
    overlap: usize,
    include_headers: bool,
    delimiter: Option<u8>,
    encoding: &str,
    skip_empty_rows: bool,
) -> Result<CsvStreamIterator> {
    let lower_ext = file_path.to_ascii_lowercase();
    if !lower_ext.ends_with(".csv") && !lower_ext.ends_with(".tsv") {
        return Err(ChunkError::InvalidArg(format!(
            "Expected .csv or .tsv file path, got: {file_path}"
        )));
    }
    match mode {
        "row" | "default" | "sliding_window" | "page_aware" => {}
        _ => {
            return Err(ChunkError::InvalidArg(
                "mode must be 'row', 'default', 'sliding_window', or 'page_aware' for CSV"
                    .to_string(),
            ))
        }
    }
    if rows_per_chunk == 0 {
        return Err(ChunkError::InvalidArg(
            "rows_per_chunk must be greater than 0".to_string(),
        ));
    }
    if window_size == 0 {
        return Err(ChunkError::InvalidArg(
            "window_size must be greater than 0".to_string(),
        ));
    }
    if overlap >= window_size {
        return Err(ChunkError::InvalidArg(
            "overlap must be less than window_size".to_string(),
        ));
    }

    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let file_path = file_path.to_string();
    let mode = mode.to_string();
    let encoding = encoding.to_string();
    let thread = thread::spawn(move || {
        let result = run_worker(
            file_path,
            mode,
            rows_per_chunk,
            window_size,
            overlap,
            include_headers,
            delimiter,
            encoding,
            skip_empty_rows,
            worker_sender,
        );
        if let Err(err) = result {
            let _ = sender.send(Err(err));
        }
    });

    Ok(CsvStreamIterator {
        receiver,
        _thread: thread,
        done: false,
    })
}
