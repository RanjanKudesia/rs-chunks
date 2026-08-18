//! JSON / JSONL / NDJSON extraction: reduce the input to *records*, render each
//! as a readable markdown section, then chunk via the Markdown chunker. Mirrors
//! the `.eml`/`.odt` assemble-then-chunk pattern. No new dependency (serde_json).

use serde_json::Value;

/// Max nesting depth we expand in the markdown; deeper values are compacted to
/// inline JSON so output stays readable and bounded.
const MAX_RENDER_DEPTH: usize = 6;

/// Common envelope keys whose array value holds the real records
/// (GeoJSON `features`, typical API `data`/`results`/…).
const ENVELOPE_KEYS: &[&str] = &["features", "data", "results", "items", "records", "rows"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JsonKind {
    /// A single `.json` document.
    Document,
    /// Line-delimited `.jsonl` / `.ndjson`.
    Lines,
}

pub struct JsonDoc {
    pub markdown: String,
    pub record_count: usize,
    /// The markdown block each record starts at, in order — what turns a
    /// chunk's block span into the records it covers ([#46](TECH_DEBT.md)).
    pub record_starts: Vec<usize>,
    /// "array" | "object" | "lines"
    pub top_level: &'static str,
    pub envelope_key: Option<String>,
}

fn scalar_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

/// Render a key/value pair into `out` as one or more indented bullet lines.
fn render_kv(key: &str, value: &Value, indent: usize, depth: usize, out: &mut Vec<String>) {
    let pad = "  ".repeat(indent);
    match value {
        v if is_scalar(v) => out.push(format!("{pad}- **{key}:** {}", scalar_to_string(v))),
        Value::Array(arr) if arr.iter().all(is_scalar) => {
            let joined = arr
                .iter()
                .map(scalar_to_string)
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("{pad}- **{key}:** {joined}"));
        }
        Value::Array(arr) => {
            if depth >= MAX_RENDER_DEPTH {
                out.push(format!("{pad}- **{key}:** {}", compact(value)));
                return;
            }
            out.push(format!("{pad}- **{key}:**"));
            for (i, item) in arr.iter().enumerate() {
                render_kv(&format!("[{i}]"), item, indent + 1, depth + 1, out);
            }
        }
        Value::Object(map) => {
            if depth >= MAX_RENDER_DEPTH {
                out.push(format!("{pad}- **{key}:** {}", compact(value)));
                return;
            }
            out.push(format!("{pad}- **{key}:**"));
            for (k, v) in map {
                render_kv(k, v, indent + 1, depth + 1, out);
            }
        }
        _ => {}
    }
}

fn compact(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    // Bound pathological single-value blowups.
    if s.len() > 4000 {
        format!("{}…", &s[..crate::shared::floor_char_boundary(&s, 4000)])
    } else {
        s
    }
}

/// Render one record as a single self-contained markdown block (one bullet list
/// for objects/arrays, one paragraph for scalars). No per-record heading: each
/// record then maps to exactly one chunk (verified against the Markdown chunker),
/// so record data retrieves cleanly for RAG without noise "Record N" chunks.
fn render_record(value: &Value) -> String {
    let mut out: Vec<String> = Vec::new();
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "(empty object)".to_string();
            }
            for (k, v) in map {
                render_kv(k, v, 0, 0, &mut out);
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                return "(empty array)".to_string();
            }
            for (i, item) in arr.iter().enumerate() {
                render_kv(&format!("[{i}]"), item, 0, 0, &mut out);
            }
        }
        scalar => return scalar_to_string(scalar),
    }
    out.join("\n")
}

/// The markdown block index each rendered record starts at.
///
/// Counted with the chunker's own block parser, not by splitting on blank
/// lines: a record can render to several blocks (a paragraph and a table), and
/// an oversized one is split further still. Records are joined with a blank
/// line, so no block ever spans two of them and counting per record and summing
/// gives the same list the whole document produces —
/// `record_starts_match_the_documents_own_blocks` asserts exactly that.
fn block_starts(rendered: &[String]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(rendered.len());
    let mut total = 0usize;
    for record in rendered {
        starts.push(total);
        total += crate::formats::md::common::parse_markdown_blocks(record).len();
    }
    starts
}

/// Determine the record set for a parsed `.json` value.
fn document_records(root: &Value) -> (Vec<&Value>, &'static str, Option<String>) {
    match root {
        Value::Array(arr) => (arr.iter().collect(), "array", None),
        Value::Object(map) => {
            // Envelope: a single dominant array-of-values under a known key.
            for key in ENVELOPE_KEYS {
                if let Some(Value::Array(arr)) = map.get(*key) {
                    if !arr.is_empty() {
                        return (arr.iter().collect(), "object", Some((*key).to_string()));
                    }
                }
            }
            (vec![root], "object", None)
        }
        other => (vec![other], "object", None),
    }
}

/// Parse a `.json` document. Returns a clean error string on malformed input or
/// input too deeply nested (serde_json's recursion limit rejects the ~100k-deep
/// torture case as an error rather than overflowing the stack).
pub fn parse_document(raw: &[u8]) -> Result<JsonDoc, String> {
    let root: Value = serde_json::from_slice(raw).map_err(|e| format!("Invalid JSON: {e}"))?;
    let (records, top_level, envelope_key) = document_records(&root);
    let record_count = records.len();
    let rendered: Vec<String> = records.iter().map(|v| render_record(v)).collect();
    let record_starts = block_starts(&rendered);
    Ok(JsonDoc {
        markdown: rendered.join("\n\n").trim().to_string(),
        record_count,
        record_starts,
        top_level,
        envelope_key,
    })
}

/// Parse `.jsonl` / `.ndjson`: one record per non-blank line. A malformed line
/// is rendered as a raw paragraph rather than failing the whole file.
pub fn parse_lines(raw: &[u8]) -> JsonDoc {
    let text = String::from_utf8_lossy(raw);
    let mut sections: Vec<String> = Vec::new();
    let mut count = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        count += 1;
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => sections.push(render_record(&v)),
            Err(_) => sections.push(trimmed.to_string()),
        }
    }
    let record_starts = block_starts(&sections);
    JsonDoc {
        markdown: sections.join("\n\n").trim().to_string(),
        record_count: count,
        record_starts,
        top_level: "lines",
        envelope_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::md::common::parse_markdown_blocks;

    /// The mapping is only sound if counting blocks per record and summing
    /// gives the same total the whole document produces. Records are joined by
    /// a blank line so no block can span two of them — asserted here over every
    /// shipped fixture rather than argued.
    #[test]
    fn record_starts_match_the_documents_own_blocks() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test_files");
        // The fixture corpus lives at the workspace root and is not shipped with
        // the repo — corpus-less checkouts (release CI runs `--lib`) skip here.
        // When the corpus IS present, the `checked >= 10` guard below stays loud.
        if !dir.exists() {
            eprintln!("skipping: fixture corpus not present at {}", dir.display());
            return;
        }
        let mut checked = 0;
        for sub in ["json", "jsonl"] {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let Ok(raw) = std::fs::read(&path) else {
                    continue;
                };
                let name = path.to_string_lossy().to_ascii_lowercase();
                let doc = if name.ends_with(".jsonl") || name.ends_with(".ndjson") {
                    parse_lines(&raw)
                } else if name.ends_with(".json") {
                    match parse_document(&raw) {
                        Ok(doc) => doc,
                        Err(_) => continue, // adversarial fixtures are meant to fail
                    }
                } else {
                    continue;
                };
                let total = parse_markdown_blocks(&doc.markdown).len();
                assert_eq!(
                    doc.record_starts.len(),
                    doc.record_count,
                    "{}: one start per record",
                    path.display()
                );
                if let Some(last) = doc.record_starts.last() {
                    assert!(
                        *last <= total,
                        "{}: record {} starts past the document's {total} blocks",
                        path.display(),
                        doc.record_starts.len() - 1
                    );
                }
                assert!(
                    doc.record_starts.windows(2).all(|w| w[0] <= w[1]),
                    "{}: record starts run backwards",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked >= 10, "only {checked} fixtures checked");
    }
}
