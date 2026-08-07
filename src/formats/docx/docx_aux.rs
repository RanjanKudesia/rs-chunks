//! Auxiliary-part helpers for DOCX: footnote/endnote XML extraction and
//! bounded zip-entry reads.

use quick_xml::escape::unescape;
use std::collections::HashMap;
use std::io::Read;
use zip::ZipArchive;

pub(super) fn extract_text_from_xml(xml: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut cursor = 0usize;

    while let Some(start) = find_next_wt_tag_start(xml, cursor) {
        let open_end = xml[start..]
            .find('>')
            .map(|i| start + i + 1)
            .ok_or_else(|| "Malformed text tag in DOCX XML".to_string())?;

        if open_end >= 2 && xml[open_end - 2..open_end].starts_with("/>") {
            cursor = open_end;
            continue;
        }

        let close = xml[open_end..]
            .find("</w:t>")
            .map(|i| open_end + i)
            .ok_or_else(|| {
                let start_snippet = start.saturating_sub(30);
                let end_snippet = (open_end + 60).min(xml.len());
                let snippet = &xml[start_snippet..end_snippet];
                format!("Unclosed text tag in DOCX XML near: {}", snippet)
            })?;

        let raw = &xml[open_end..close];
        let decoded = unescape(raw)
            .map_err(|e| format!("Failed to decode XML entities: {e}"))?
            .into_owned();

        if !decoded.trim().is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(decoded.trim());
        }

        cursor = close + "</w:t>".len();
    }

    Ok(out)
}

pub(super) fn find_next_wt_tag_start(xml: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some(rel_start) = xml[cursor..].find("<w:t") {
        let start = cursor + rel_start;
        let next_index = start + 4;
        let next = xml.as_bytes().get(next_index).copied();

        if matches!(
            next,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            return Some(start);
        }

        cursor = next_index;
    }
    None
}

pub(super) fn extract_notes_map(xml: &str, tag: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let open_marker = format!("<w:{tag}");
    let close_marker = format!("</w:{tag}>");
    let mut cursor = 0usize;

    while let Some(rel_start) = xml[cursor..].find(&open_marker) {
        let start = cursor + rel_start;
        let end = match xml[start..].find(close_marker.as_str()) {
            Some(i) => start + i + close_marker.len(),
            None => break,
        };

        let block = &xml[start..end];

        // Skip the auto-generated separator entries (w:type="separator",
        // "continuationSeparator", "continuationNotice") that Word inserts at
        // the top of footnotes.xml / endnotes.xml — they carry no author
        // content and would only add noise if surfaced to consumers.
        let note_type = first_tag_attr(block, "w:type");
        let is_separator = matches!(
            note_type.as_deref(),
            Some("separator") | Some("continuationSeparator") | Some("continuationNotice")
        );
        if is_separator {
            cursor = end;
            continue;
        }

        if let Some(id) = first_tag_attr(block, "w:id") {
            if let Ok(text) = extract_text_from_xml(block) {
                let clean = text.trim().to_string();
                if !clean.is_empty() {
                    out.insert(id, clean);
                }
            }
        }

        cursor = end;
    }

    out
}

/// Read an attribute value out of the first `<…>` tag in `block`. Returns
/// `None` when the tag has no such attribute. Tolerates both `"` and `'`
/// quoted values, but assumes the opening tag does not contain a stray `>`
/// inside an attribute value (always true for well-formed DOCX XML).
pub(super) fn first_tag_attr(block: &str, attr: &str) -> Option<String> {
    let tag_end = block.find('>')?;
    let header = &block[..tag_end];
    let needle = format!("{attr}=");
    let mut search_from = 0usize;
    while let Some(rel) = header[search_from..].find(&needle) {
        let idx = search_from + rel;
        // Ensure this match is at a token boundary (preceded by whitespace,
        // `<`, or string start) so we don't match `xyz{attr}=` substrings.
        let prev = idx.checked_sub(1).and_then(|p| header.as_bytes().get(p));
        let boundary_ok = matches!(
            prev,
            None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'/')
        );
        if !boundary_ok {
            search_from = idx + needle.len();
            continue;
        }
        let after = &header[idx + needle.len()..];
        let quote_char = after.chars().next()?;
        if quote_char != '"' && quote_char != '\'' {
            return None;
        }
        let rest = &after[1..];
        let end_q = rest.find(quote_char)?;
        return Some(rest[..end_q].to_string());
    }
    None
}

pub(super) fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> Result<Option<String>, String> {
    match archive.by_name(name) {
        Ok(mut file) => {
            let size = file.size();
            if size > max_bytes {
                return Err(format!(
                    "{name} is too large after decompression: {} bytes (limit: {} bytes)",
                    size, max_bytes
                ));
            }
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| format!("Failed to read {name}: {e}"))?;
            Ok(Some(content))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("Failed to open {name}: {e}")),
    }
}

pub(super) fn read_first_prefixed_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    prefix: &str,
    max_bytes: u64,
) -> Result<Option<String>, String> {
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to inspect zip entry at index {i}: {e}"))?;
        if file.name().starts_with(prefix) {
            let size = file.size();
            if size > max_bytes {
                return Err(format!(
                    "{} is too large after decompression: {} bytes (limit: {} bytes)",
                    file.name(),
                    size,
                    max_bytes
                ));
            }
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| format!("Failed to read {}: {e}", file.name()))?;
            return Ok(Some(content));
        }
    }
    Ok(None)
}

pub(super) fn count_prefixed_entries<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    prefix: &str,
) -> Result<usize, String> {
    let mut count = 0usize;
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to inspect zip entry at index {i}: {e}"))?;
        if file.name().starts_with(prefix) {
            count += 1;
        }
    }
    Ok(count)
}
