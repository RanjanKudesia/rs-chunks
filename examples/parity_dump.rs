//! Dumps chunks-rs output for every implemented fixture × mode as JSON lines,
//! for cross-checking against the Python engine. Each line is either
//!   {"path": <abs>, "ext": <ext>, "mode": <mode>, "ok": bool, "chunks": [...]}
//! or, once per file,
//!   {"path": <abs>, "ext": <ext>, "mode": "__markdown__", "ok": bool, "markdown": <str>}
//!
//! `get_markdown` is dumped because chunks alone were not enough: TECH_DEBT #75
//! was a whole public API corrupting text while this harness stayed at 0
//! mismatches, because it never looked at markdown.

use std::path::PathBuf;

use chunks_rs::{get_chunks, get_markdown};

const PROSE_EXTS: &[&str] = &[
    "md", "txt", "html", "htm", "json", "jsonl", "ndjson", "eml", "mbox", "odt", "odp", "msg",
    "ipynb", "rtf", "pdf", "epub", "docx", "docm", "dotx", "dotm", "doc", "ppt", "pptx", "potx", "potm", "ppsx", "ppsm",
];
const DELIM_EXTS: &[&str] = &["csv", "tsv"];
const SHEET_EXTS: &[&str] = &["xlsx", "xls", "xlsm", "xlsb", "ods", "xltx", "xltm"];

const PROSE_MODES: &[&str] = &["default", "section", "semantic", "sentence", "page_aware", "sliding_window"];
const DELIM_MODES: &[&str] = &["default", "sliding_window", "page_aware"];
const SHEET_MODES: &[&str] = &["default", "table", "sheet", "semantic", "page_aware", "sliding_window"];

fn test_files_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("test_files")
}

fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

fn main() {
    let root = test_files_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();

    for path in files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let modes: &[&str] = if PROSE_EXTS.contains(&ext.as_str()) {
            PROSE_MODES
        } else if DELIM_EXTS.contains(&ext.as_str()) {
            DELIM_MODES
        } else if SHEET_EXTS.contains(&ext.as_str()) {
            SHEET_MODES
        } else {
            continue; // not yet implemented in chunks-rs
        };
        // Skip multi-MB stress fixtures (e.g. 100mb.doc) — they are validated
        // separately; re-parsing them once per mode makes the sweep intractable.
        if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
            continue;
        }
        let p = path.to_str().unwrap();
        // One markdown record per file — the surface #75 hid in.
        let line = match get_markdown(p) {
            Ok(md) => serde_json::json!({
                "path": p, "ext": ext, "mode": "__markdown__", "ok": true, "markdown": md,
            }),
            Err(e) => serde_json::json!({
                "path": p, "ext": ext, "mode": "__markdown__", "ok": false, "error": e.to_string(),
            }),
        };
        println!("{}", serde_json::to_string(&line).unwrap());
        for mode in modes {
            match get_chunks(p, mode, 3, 1, 3, 15) {
                Ok(chunks) => {
                    let line = serde_json::json!({
                        "path": p, "ext": ext, "mode": mode, "ok": true, "chunks": chunks,
                    });
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
                Err(e) => {
                    let line = serde_json::json!({
                        "path": p, "ext": ext, "mode": mode, "ok": false, "error": e.to_string(),
                    });
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
        }
    }
}
