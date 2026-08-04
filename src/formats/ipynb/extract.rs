//! Jupyter notebook (.ipynb) → markdown assembly.
//!
//! Parses the notebook JSON and renders cells (in order) into a faithful markdown
//! document: markdown cells pass through; code cells become fenced code blocks in
//! the kernel language followed by their (text) outputs; images (base64 in outputs
//! and markdown attachments) are extracted. The markdown then feeds the MD chunker.

use base64::Engine;
use serde_json::Value;

/// Per-output text is capped to avoid a single huge output bloating chunks.
const MAX_OUTPUT_CHARS: usize = 4000;

#[derive(Default)]
pub struct IpynbDoc {
    pub language: Option<String>,
    pub kernel: Option<String>,
    pub nbformat: Option<String>,
    pub cell_count: usize,
    pub code_cell_count: usize,
    pub markdown_cell_count: usize,
    pub markdown: String,
    /// Extracted images: name → bytes.
    pub images: Vec<(String, Vec<u8>)>,
}

/// A cell `source`/`text` is a list of line strings (or occasionally one string).
fn join_source(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Strip ANSI SGR escape sequences (error tracebacks are colourised).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC [ ... m
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'm' {
                    j += 1;
                }
                i = (j + 1).min(bytes.len());
                continue;
            }
        }
        // copy this UTF-8 char whole
        let ch_len = utf8_len(bytes[i]);
        if let Ok(s) = std::str::from_utf8(&bytes[i..(i + ch_len).min(bytes.len())]) {
            out.push_str(s);
        }
        i += ch_len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

fn cap(s: &str) -> String {
    if s.chars().count() > MAX_OUTPUT_CHARS {
        let capped: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
        format!("{capped}\n…[output truncated]")
    } else {
        s.to_string()
    }
}

/// Light HTML → text for `text/html` outputs (tables etc.).
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_b64_image(data: &str) -> Option<Vec<u8>> {
    // Notebook image payloads may contain newlines.
    let clean: String = data.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD.decode(clean).ok()
}

pub fn extract(bytes: &[u8]) -> Result<IpynbDoc, String> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("Not a valid .ipynb (JSON) file: {e}"))?;

    let mut doc = IpynbDoc {
        nbformat: root
            .get("nbformat")
            .map(|v| format!("{}.{}", v, root.get("nbformat_minor").unwrap_or(&Value::from(0)))),
        ..Default::default()
    };

    // Language / kernel.
    if let Some(meta) = root.get("metadata") {
        doc.language = meta
            .pointer("/kernelspec/language")
            .and_then(|v| v.as_str())
            .or_else(|| meta.pointer("/language_info/name").and_then(|v| v.as_str()))
            .map(String::from);
        doc.kernel = meta
            .pointer("/kernelspec/display_name")
            .or_else(|| meta.pointer("/kernelspec/name"))
            .and_then(|v| v.as_str())
            .map(String::from);
    }
    let lang = doc.language.clone().unwrap_or_default();

    // Cells: nbformat 4 (top-level `cells`) or nbformat 3 (`worksheets[].cells`).
    let cells: Vec<&Value> = if let Some(c) = root.get("cells").and_then(|v| v.as_array()) {
        c.iter().collect()
    } else if let Some(ws) = root.get("worksheets").and_then(|v| v.as_array()) {
        ws.iter()
            .filter_map(|w| w.get("cells").and_then(|v| v.as_array()))
            .flatten()
            .collect()
    } else {
        Vec::new()
    };

    let mut md = String::new();
    let mut image_counter = 0usize;

    for cell in cells {
        doc.cell_count += 1;
        let cell_type = cell.get("cell_type").and_then(|v| v.as_str()).unwrap_or("");
        let source = cell
            .get("source")
            .or_else(|| cell.get("input"))
            .map(join_source)
            .unwrap_or_default();

        match cell_type {
            "markdown" => {
                doc.markdown_cell_count += 1;
                // Resolve markdown-cell attachments (![](attachment:name)).
                let mut body = source.clone();
                if let Some(atts) = cell.get("attachments").and_then(|v| v.as_object()) {
                    for (name, mimes) in atts {
                        if let Some(obj) = mimes.as_object() {
                            for (mime, data) in obj {
                                if mime.starts_with("image/") {
                                    if let Some(img) = data.as_str().and_then(decode_b64_image) {
                                        let ext = mime.rsplit('/').next().unwrap_or("png");
                                        let fname = format!("attachment_{name}.{ext}");
                                        body = body.replace(
                                            &format!("attachment:{name}"),
                                            &fname,
                                        );
                                        doc.images.push((fname, img));
                                    }
                                }
                            }
                        }
                    }
                }
                md.push_str(body.trim_end());
                md.push_str("\n\n");
            }
            "code" => {
                doc.code_cell_count += 1;
                if !source.trim().is_empty() {
                    md.push_str(&format!("```{lang}\n{}\n```\n\n", source.trim_end()));
                }
                render_outputs(cell, &mut md, &mut doc.images, &mut image_counter);
            }
            "heading" => {
                // Legacy nbformat 3 heading cell.
                let level = cell.get("level").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                let hashes = "#".repeat(level.clamp(1, 6));
                md.push_str(&format!("{hashes} {}\n\n", source.trim()));
            }
            _ => {
                // raw / unknown — include as plain text.
                if !source.trim().is_empty() {
                    md.push_str(source.trim_end());
                    md.push_str("\n\n");
                }
            }
        }
    }

    doc.markdown = md.trim().to_string();
    Ok(doc)
}

fn render_outputs(
    cell: &Value,
    md: &mut String,
    images: &mut Vec<(String, Vec<u8>)>,
    image_counter: &mut usize,
) {
    let Some(outputs) = cell.get("outputs").and_then(|v| v.as_array()) else {
        return;
    };
    for out in outputs {
        let otype = out.get("output_type").and_then(|v| v.as_str()).unwrap_or("");
        match otype {
            "stream" => {
                let text = out.get("text").map(join_source).unwrap_or_default();
                if !text.trim().is_empty() {
                    md.push_str(&format!("```\n{}\n```\n\n", cap(text.trim_end())));
                }
            }
            "execute_result" | "display_data" => {
                let data = out.get("data");
                // Image?
                if let Some(img) = data
                    .and_then(|d| d.get("image/png"))
                    .and_then(|v| v.as_str())
                    .and_then(decode_b64_image)
                {
                    *image_counter += 1;
                    let name = format!("output_image_{image_counter}.png");
                    md.push_str(&format!("![]({name})\n\n"));
                    images.push((name, img));
                }
                // Text representation, richest first.
                //
                // `text/plain` was preferred unconditionally, which loses badly
                // when a renderer supplies something better: nbc_rst_output.ipynb
                // has text/plain `<__main__.Note at 0x7f457c0250d0>` — a repr
                // address, pure noise — next to text/html "Testing testing" and a
                // text/x-rst note block. Jupyter's own front-ends pick the richest
                // available; so do we, falling back whenever the richer form
                // converts to nothing. (#43)
                let rich = data
                    .and_then(|d| d.get("text/html"))
                    .map(join_source)
                    .map(|h| html_to_text(&h))
                    .filter(|t| !t.trim().is_empty())
                    .or_else(|| {
                        data.and_then(|d| d.get("text/markdown"))
                            .map(join_source)
                            .filter(|t| !t.trim().is_empty())
                    })
                    .or_else(|| {
                        data.and_then(|d| d.get("text/x-rst"))
                            .map(join_source)
                            .filter(|t| !t.trim().is_empty())
                    });
                if let Some(text) = rich {
                    md.push_str(&format!("{}\n\n", cap(text.trim())));
                } else if let Some(t) = data.and_then(|d| d.get("text/plain")).map(join_source) {
                    if !t.trim().is_empty() {
                        md.push_str(&format!("```\n{}\n```\n\n", cap(t.trim_end())));
                    }
                }
            }
            "error" => {
                let tb = out
                    .get("traceback")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let tb = strip_ansi(&tb);
                if !tb.trim().is_empty() {
                    md.push_str(&format!("```\n{}\n```\n\n", cap(tb.trim())));
                }
            }
            _ => {} // unknown/malformed output type — skip gracefully
        }
    }
}

pub fn to_markdown(doc: &IpynbDoc) -> String {
    doc.markdown.clone()
}
