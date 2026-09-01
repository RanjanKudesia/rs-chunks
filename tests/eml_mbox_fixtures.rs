//! Dedicated fixture-driven tests for the email family (.eml / .mbox).
//!
//! Requires the workspace fixture corpus at ../test_files (not shipped with the
//! crate). Like tests/dispatch_smoke.rs, a missing/moved corpus fails loudly
//! via the fixture-count guards instead of silently testing nothing.

use std::path::PathBuf;

use chunks_rs::{get_chunks, get_chunks_from_bytes, get_markdown};

fn fixtures(dir: &str, ext: &str) -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join(dir);
    let mut out: Vec<PathBuf> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case(ext))
                == Some(true)
                && std::fs::metadata(p)
                    .map(|m| m.len() < 10 * 1024 * 1024)
                    .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn eml_corpus_chunks_cleanly() {
    let files = fixtures("eml", "eml");
    // Guard: a missing corpus must fail loudly, not pass vacuously.
    assert!(
        files.len() >= 10,
        "expected >= 10 .eml fixtures, found {}",
        files.len()
    );

    let mut ok = 0;
    let mut total_chunks = 0;
    for path in &files {
        let p = path.to_str().unwrap();
        for mode in ["default", "semantic"] {
            match get_chunks(p, mode, 3, 1, 3, 15) {
                Ok(chunks) => {
                    if mode == "default" {
                        ok += 1;
                        total_chunks += chunks.len();
                    }
                    for c in &chunks {
                        assert!(!c.content_type.is_empty(), "empty content_type: {p}");
                        assert!(c.metadata.is_object(), "metadata not an object: {p}");
                    }
                }
                Err(e) => {
                    // Malformed fixtures (stalwart_malformed_*) may fail — cleanly.
                    assert!(!e.to_string().is_empty(), "empty error for {p}");
                }
            }
        }
    }
    // The corpus is mostly well-formed mail: parsing must actually work, not
    // just not-crash.
    assert!(
        ok * 2 >= files.len(),
        "only {ok}/{} .eml fixtures chunked Ok",
        files.len()
    );
    assert!(
        total_chunks > 0,
        "no chunks produced from the whole .eml corpus"
    );
}

#[test]
fn eml_markdown_and_bytes_roundtrip() {
    let files = fixtures("eml", "eml");
    assert!(
        files.len() >= 10,
        "expected >= 10 .eml fixtures, found {}",
        files.len()
    );

    let mut md_nonempty = 0;
    for path in files.iter().take(10) {
        let p = path.to_str().unwrap();
        if let Ok(md) = get_markdown(p) {
            if !md.trim().is_empty() {
                md_nonempty += 1;
            }
        }
        // Bytes API must agree with the path API (same routing, no temp file).
        let bytes = std::fs::read(p).unwrap();
        let via_path = get_chunks(p, "default", 3, 1, 3, 15);
        let via_bytes = get_chunks_from_bytes(&bytes, "x.eml", "default", 3, 1, 3, 15);
        match (via_path, via_bytes) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "bytes != path for {p}"),
            (Err(_), Err(_)) => {}
            (a, b) => panic!("path/bytes disagree on success for {p}: {a:?} vs {b:?}"),
        }
    }
    assert!(
        md_nonempty > 0,
        "no .eml fixture produced non-empty markdown"
    );
}

#[test]
fn mbox_corpus_chunks_cleanly() {
    let files = fixtures("mbox", "mbox");
    assert!(
        files.len() >= 5,
        "expected >= 5 .mbox fixtures, found {}",
        files.len()
    );

    let mut total_chunks = 0;
    for path in &files {
        let p = path.to_str().unwrap();
        match get_chunks(p, "default", 3, 1, 3, 15) {
            Ok(chunks) => {
                total_chunks += chunks.len();
                for c in &chunks {
                    assert!(!c.content_type.is_empty(), "empty content_type: {p}");
                    assert!(c.metadata.is_object(), "metadata not an object: {p}");
                }
            }
            Err(e) => assert!(!e.to_string().is_empty(), "empty error for {p}"),
        }
    }
    assert!(
        total_chunks > 0,
        "no chunks produced from the whole .mbox corpus"
    );
}
