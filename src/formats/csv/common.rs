use encoding_rs::Encoding;
use serde_json::Value;
use std::fs;

pub const CT_ROW_GROUP: &str = "row_group";
pub const CT_ROW_WINDOW: &str = "row_window";

#[derive(Debug, Clone)]
pub struct CsvChunkRecord {
    pub content: String,
    pub content_type: String,
    pub metadata: Value,
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    }
}

fn encoding_for_label(label: &str) -> Result<&'static Encoding, String> {
    Encoding::for_label(label.as_bytes()).ok_or_else(|| format!("Unsupported encoding: {label}"))
}

fn decode_bytes(bytes: &[u8], encoding: &str) -> Result<String, String> {
    match encoding.to_ascii_lowercase().as_str() {
        "utf-8" => {
            let encoding = encoding_for_label("utf-8")?;
            let (decoded, _, had_errors) = encoding.decode(bytes);
            if had_errors {
                return Err("Failed to decode CSV bytes as utf-8 without replacement".to_string());
            }
            Ok(decoded.into_owned())
        }
        "utf-8-bom" => {
            let encoding = encoding_for_label("utf-8")?;
            let (decoded, _, had_errors) = encoding.decode(strip_bom(bytes));
            if had_errors {
                return Err("Failed to decode CSV bytes as utf-8-bom without replacement".to_string());
            }
            Ok(decoded.into_owned())
        }
        "latin-1" => {
            let encoding = encoding_for_label("iso-8859-1")?;
            let (decoded, _, had_errors) = encoding.decode(bytes);
            if had_errors {
                return Err("Failed to decode CSV bytes as latin-1 without replacement".to_string());
            }
            Ok(decoded.into_owned())
        }
        "windows-1252" => {
            let encoding = encoding_for_label("windows-1252")?;
            let (decoded, _, had_errors) = encoding.decode(bytes);
            if had_errors {
                return Err("Failed to decode CSV bytes as windows-1252 without replacement".to_string());
            }
            Ok(decoded.into_owned())
        }
        other => Err(format!("Unsupported encoding: {other}")),
    }
}

fn is_comment_or_empty_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn first_data_line(text: &str) -> Option<&str> {
    text.lines().find(|line| !is_comment_or_empty_line(line))
}

pub fn detect_delimiter(first_line: &str) -> u8 {
    let candidates = [(',', 0usize), ('\t', 1usize), (';', 2usize), ('|', 3usize)];
    let mut counts = [0usize; 4];
    let mut in_quotes = false;
    let chars: Vec<char> = first_line.chars().collect();
    let mut idx = 0usize;

    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '"' {
            if idx + 1 < chars.len() && chars[idx + 1] == '"' {
                idx += 2;
                continue;
            }
            in_quotes = !in_quotes;
            idx += 1;
            continue;
        }
        if !in_quotes {
            for (candidate, slot) in candidates {
                if ch == candidate {
                    counts[slot] += 1;
                }
            }
        }
        idx += 1;
    }

    let mut winner = 0usize;
    for i in 1..counts.len() {
        if counts[i] > counts[winner] {
            winner = i;
        }
    }
    candidates[winner].0 as u8
}

pub fn read_to_utf8(file_path: &str, encoding: &str) -> Result<String, String> {
    let bytes = fs::read(file_path).map_err(|e| format!("Failed to read file: {e}"))?;
    decode_bytes(&bytes, encoding)
}

pub fn read_first_data_line(file_path: &str, encoding: &str) -> Result<Option<String>, String> {
    let bytes = fs::read(file_path).map_err(|e| format!("Failed to read file: {e}"))?;
    let text = decode_bytes(&bytes, encoding)?;
    Ok(first_data_line(&text).map(|line| line.to_string()))
}

/// Decode raw CSV bytes to UTF-8 (no filesystem) — the wasm/bytes path.
pub fn decode_to_utf8(bytes: &[u8], encoding: &str) -> Result<String, String> {
    decode_bytes(bytes, encoding)
}

/// First non-comment/non-empty line of raw CSV bytes (for delimiter detection).
pub fn first_data_line_of(bytes: &[u8], encoding: &str) -> Result<Option<String>, String> {
    let text = decode_bytes(bytes, encoding)?;
    Ok(first_data_line(&text).map(|line| line.to_string()))
}

fn row_label(headers: &[String], idx: usize) -> String {
    headers
        .get(idx)
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("Column {}", idx + 1))
}

pub fn serialize_row_kv(headers: &[String], row: &[String]) -> String {
    let width = headers.len().max(row.len());
    (0..width)
        .map(|idx| format!("{}: {}", row_label(headers, idx), row.get(idx).cloned().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn serialize_row_values(row: &[String]) -> String {
    row.join(" | ")
}