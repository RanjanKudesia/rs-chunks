// src/extensions/html/images.rs

use std::path::Path;

#[derive(Debug, Clone)]
pub struct HtmlImageInfo {
    pub hash_name: String,
    pub alt_text: Option<String>,
}

/// Returns `"{16hexchars}.{ext}"` for web-renderable formats; None otherwise.
pub fn image_hash_name(bytes: &[u8], ext: &str) -> Option<String> {
    crate::image_naming::name_with_ext(bytes, ext)
}

/// Extracts the value of an HTML attribute from a raw tag slice like
/// `<img src="..." alt="..."/>`. Handles both `attr="value"` and `attr='value'`.
/// The search is case-insensitive on the attribute name.
pub fn get_tag_attribute(tag: &str, attr_name: &str) -> Option<String> {
    let lower_tag = tag.to_ascii_lowercase();
    let lower_attr = attr_name.to_ascii_lowercase();

    for quote in ['"', '\''] {
        let pattern = format!("{lower_attr}={quote}");
        if let Some(pos) = lower_tag.find(&pattern) {
            let value_start = pos + pattern.len();
            if let Some(end_offset) = tag[value_start..].find(quote) {
                return Some(tag[value_start..value_start + end_offset].to_string());
            }
        }
    }
    None
}

/// Determine the file extension for a data URI mime type.
/// Returns None for unsupported/non-image types.
fn data_uri_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpeg"),
        "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Decode a `data:image/...;base64,...` URI into (bytes, extension).
fn decode_data_uri(src: &str) -> Option<(Vec<u8>, &'static str)> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let rest = src.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let (mime_part, encoding) = meta.split_once(';').unwrap_or((meta, ""));
    if encoding != "base64" {
        return None;
    }
    let ext = data_uri_ext(&mime_part.to_ascii_lowercase())?;
    let bytes = STANDARD.decode(data.trim()).ok()?;
    Some((bytes, ext))
}

/// Resolve a local file reference relative to the HTML file's parent directory.
/// Returns (bytes, extension) or None if the path is remote, unsupported, or unreadable.
fn resolve_local_file(src: &str, html_dir: &Path) -> Option<(Vec<u8>, String)> {
    // Skip remote URLs and protocol-relative URLs.
    if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("//") {
        return None;
    }
    let img_path = if src.starts_with('/') {
        Path::new(src).to_path_buf()
    } else {
        html_dir.join(src)
    };
    let ext = img_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    // Only supported web-renderable formats.
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return None;
    }
    let bytes = std::fs::read(&img_path).ok()?;
    Some((bytes, ext))
}

/// Scan `html` for `<img>` tags and extract embedded / local images.
///
/// - `file_path`: the path to the HTML file; used to resolve relative src URLs.
/// - `image_out`: accumulator of `(hash_name, bytes)` pairs (deduplicated by hash).
///
/// Returns one `HtmlImageInfo` per `<img>` whose `src` could be resolved
/// (possibly pointing to the same hash as an earlier image if the source is identical).
pub fn collect_html_images(
    html: &str,
    file_path: &str,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<HtmlImageInfo> {
    let html_dir = Path::new(file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let mut result = Vec::new();
    let mut i = 0usize;

    while i < html.len() {
        // Look for the next `<img` (case-insensitive).
        let Some(tag_start) = find_img_start(html, i) else { break };

        // Find the closing `>` of this tag.
        let Some(tag_end) = find_tag_close(html, tag_start) else {
            i = tag_start + 4;
            continue;
        };

        let tag_slice = &html[tag_start..tag_end];

        let Some(src) = get_tag_attribute(tag_slice, "src") else {
            i = tag_end;
            continue;
        };

        let alt = get_tag_attribute(tag_slice, "alt").filter(|s| !s.trim().is_empty());

        let (img_bytes, ext): (Vec<u8>, String) = if src.starts_with("data:") {
            match decode_data_uri(&src) {
                Some((b, e)) => (b, e.to_string()),
                None => { i = tag_end; continue; }
            }
        } else {
            match resolve_local_file(&src, html_dir) {
                Some(pair) => pair,
                None => { i = tag_end; continue; }
            }
        };

        let Some(hash_name) = image_hash_name(&img_bytes, &ext) else {
            i = tag_end;
            continue;
        };

        if !image_out.iter().any(|(n, _)| n == &hash_name) {
            image_out.push((hash_name.clone(), img_bytes));
        }

        result.push(HtmlImageInfo { hash_name, alt_text: alt });
        i = tag_end;
    }

    result
}

/// Find the byte offset where the next `<img` tag begins, case-insensitively, from `from`.
fn find_img_start(html: &str, from: usize) -> Option<usize> {
    let lower = html[from..].to_ascii_lowercase();
    lower.find("<img").map(|rel| from + rel)
}

/// Find the byte offset just after the `>` that closes the tag starting at `start`.
fn find_tag_close(html: &str, start: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut i = start;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        match (in_quote, bytes[i]) {
            (None, b'"') | (None, b'\'') => { in_quote = Some(bytes[i]); i += 1; }
            (Some(q), c) if c == q => { in_quote = None; i += 1; }
            (None, b'>') => return Some(i + 1),
            _ => { i += 1; }
        }
    }
    None
}
