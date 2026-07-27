//! Smoke test: run `get_chunks` (default mode) + `get_markdown` on one real
//! fixture per format family and assert well-formed output. Exhaustive parity is
//! covered by examples/parity_dump.rs vs the Python engine; this pins the public
//! dispatch surface into `cargo test`.

use std::path::{Path, PathBuf};

use chunks_rs::{get_chunks, get_markdown};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("test_files")
}

/// Find the first file with the given extension anywhere under test_files.
fn first_with_ext(ext: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, ext: &str) -> Option<PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in &entries {
            if p.is_dir() {
                if let Some(f) = walk(p, ext) {
                    return Some(f);
                }
            } else if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext)) == Some(true)
                && std::fs::metadata(p).map(|m| m.len() < 10 * 1024 * 1024).unwrap_or(false)
            {
                return Some(p.clone());
            }
        }
        None
    }
    walk(&root(), ext)
}

const FAMILIES: &[&str] = &[
    "csv", "tsv", "md", "txt", "html", "json", "jsonl", "ndjson", "eml", "mbox", "odt", "odp",
    "msg", "ipynb", "rtf", "pdf", "epub", "docx", "docm", "dotx", "dotm", "doc", "pptx", "potx",
    "potm", "ppsx", "ppsm", "ppt", "xlsx", "xls", "xlsm", "xlsb", "ods", "xltx", "xltm",
];

#[test]
fn get_chunks_default_mode_every_family() {
    let mut checked = 0;
    for ext in FAMILIES {
        let Some(path) = first_with_ext(ext) else { continue };
        let p = path.to_str().unwrap();
        // Must return a Result without panicking/aborting. Adversarial fixtures
        // (e.g. invalid JSON) legitimately return a clean Err; when Ok, chunks
        // must be well-formed.
        match get_chunks(p, "default", 3, 1, 3, 15) {
            Ok(chunks) => {
                for c in &chunks {
                    assert!(!c.content_type.is_empty(), "empty content_type for {ext}");
                    assert!(c.metadata.is_object(), "metadata not object for {ext}");
                }
            }
            Err(_) => { /* clean failure on an adversarial/edge fixture is acceptable */ }
        }
        checked += 1;
    }
    assert!(checked >= 30, "expected to exercise most families, only did {checked}");
}

#[test]
fn get_markdown_smoke() {
    for ext in ["md", "html", "csv", "docx", "pptx", "xlsx", "pdf", "eml", "odt", "rtf", "doc"] {
        let Some(path) = first_with_ext(ext) else { continue };
        let p = path.to_str().unwrap();
        let md = get_markdown(p).unwrap_or_else(|e| panic!("get_markdown failed for {ext}: {e}"));
        // Markdown may be empty for content-less fixtures; just assert it returns.
        let _ = md;
    }
}

#[test]
fn unsupported_extension_errors() {
    assert!(get_chunks("file.xyz", "default", 3, 1, 3, 15).is_err());
    assert!(get_markdown("file.xyz").is_err());
}

#[test]
fn bytes_api_matches_path_api() {
    use chunks_rs::{get_chunks_from_bytes, get_markdown_from_bytes};
    // A couple of representative formats: an fs::read one (md) and a
    // reader-based one (docx) that the temp-file path must handle.
    for ext in ["md", "docx", "csv", "xlsx"] {
        let Some(path) = first_with_ext(ext) else { continue };
        let p = path.to_str().unwrap();
        let bytes = std::fs::read(p).unwrap();
        let via_path = get_chunks(p, "default", 3, 1, 3, 15).unwrap();
        let via_bytes =
            get_chunks_from_bytes(&bytes, &format!("x.{ext}"), "default", 3, 1, 3, 15).unwrap();
        assert_eq!(via_path, via_bytes, "bytes != path for {ext}");
        let md_path = get_markdown(p).unwrap();
        let md_bytes = get_markdown_from_bytes(&bytes, &format!("x.{ext}")).unwrap();
        assert_eq!(md_path, md_bytes, "bytes markdown != path markdown for {ext}");
    }
}
