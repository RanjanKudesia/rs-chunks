//! Dedicated fixture-driven tests for the OpenDocument family (.odt / .odp).
//!
//! Requires the workspace fixture corpus at ../test_files. A missing/moved
//! corpus fails loudly via the fixture-count guards (cf. tests/dispatch_smoke.rs).

use std::path::PathBuf;

use chunks_rs::{get_chunks, get_chunks_from_bytes, get_markdown};

fn fixtures(dir: &str, ext: &str) -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("test_files").join(dir);
    let mut out: Vec<PathBuf> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext))
                == Some(true)
                && std::fs::metadata(p).map(|m| m.len() < 10 * 1024 * 1024).unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

fn run_family(dir: &str, ext: &str, min_fixtures: usize) {
    let files = fixtures(dir, ext);
    // Guard: a missing corpus must fail loudly, not pass vacuously.
    assert!(
        files.len() >= min_fixtures,
        "expected >= {min_fixtures} .{ext} fixtures, found {}",
        files.len()
    );

    let mut ok = 0;
    let mut total_chunks = 0;
    for path in &files {
        let p = path.to_str().unwrap();
        match get_chunks(p, "default", 3, 1, 3, 15) {
            Ok(chunks) => {
                ok += 1;
                total_chunks += chunks.len();
                for c in &chunks {
                    assert!(!c.content_type.is_empty(), "empty content_type: {p}");
                    assert!(c.metadata.is_object(), "metadata not an object: {p}");
                }
            }
            // Encrypted / deliberately-invalid fixtures may fail — cleanly.
            Err(e) => assert!(!e.to_string().is_empty(), "empty error for {p}"),
        }
    }
    assert!(ok * 2 >= files.len(), "only {ok}/{} .{ext} fixtures chunked Ok", files.len());
    assert!(total_chunks > 0, "no chunks produced from the whole .{ext} corpus");
}

#[test]
fn odt_corpus_chunks_cleanly() {
    run_family("odt", "odt", 5);
}

#[test]
fn odp_corpus_chunks_cleanly() {
    run_family("odp", "odp", 5);
}

#[test]
fn odf_markdown_and_bytes_roundtrip() {
    for (dir, ext) in [("odt", "odt"), ("odp", "odp")] {
        let files = fixtures(dir, ext);
        assert!(files.len() >= 5, "expected >= 5 .{ext} fixtures, found {}", files.len());

        let mut md_nonempty = 0;
        for path in files.iter().take(5) {
            let p = path.to_str().unwrap();
            if let Ok(md) = get_markdown(p) {
                if !md.trim().is_empty() {
                    md_nonempty += 1;
                }
            }
            let bytes = std::fs::read(p).unwrap();
            let via_path = get_chunks(p, "default", 3, 1, 3, 15);
            let via_bytes = get_chunks_from_bytes(&bytes, &format!("x.{ext}"), "default", 3, 1, 3, 15);
            match (via_path, via_bytes) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "bytes != path for {p}"),
                (Err(_), Err(_)) => {}
                (a, b) => panic!("path/bytes disagree on success for {p}: {a:?} vs {b:?}"),
            }
        }
        assert!(md_nonempty > 0, "no .{ext} fixture produced non-empty markdown");
    }
}
