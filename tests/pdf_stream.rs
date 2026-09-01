//! Streaming PDF must equal batch PDF, exactly.
//!
//! The two go through different code — batch returns a `Vec`, streaming pushes
//! through a channel from a worker thread — and nothing else would catch them
//! drifting. This is the same guarantee `xlsx_stream.rs` enforces for
//! spreadsheets, for the same reason.

use std::path::PathBuf;
use std::time::Instant;

use chunks_rs::formats::pdf;

const MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

fn corpus() -> Vec<PathBuf> {
    let dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "test_files", "pdf"]
        .iter()
        .collect();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixtures")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("pdf"))
        .collect();
    files.sort();
    files
}

/// Skip the two fixtures whose size makes a 7-mode sweep take minutes; they are
/// covered on their own below.
fn is_huge(path: &PathBuf) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 8_000_000)
        .unwrap_or(false)
}

#[test]
fn stream_matches_batch_for_every_mode() {
    let mut compared = 0;
    for path in corpus().iter().filter(|p| !is_huge(p)) {
        let name = path.to_string_lossy().to_string();
        for mode in MODES {
            let batch = pdf::chunk(&name, mode, 3, 1, 3, 15);
            let streamed: Result<Vec<_>, _> = pdf::stream(&name, mode, 3, 1, 3, 15)
                .expect("construct")
                .collect();

            match (batch, streamed) {
                (Ok(batch), Ok(streamed)) => {
                    assert_eq!(batch.len(), streamed.len(), "{name} :: {mode}: chunk count");
                    for (i, (b, s)) in batch.iter().zip(&streamed).enumerate() {
                        assert_eq!(b.content, s.content, "{name} :: {mode}: content of chunk {i}");
                        assert_eq!(b.content_type, s.content_type, "{name} :: {mode}: type of chunk {i}");
                        assert_eq!(b.metadata, s.metadata, "{name} :: {mode}: metadata of chunk {i}");
                    }
                    compared += 1;
                }
                (Err(batch), Err(streamed)) => {
                    assert_eq!(
                        batch.to_string(),
                        streamed.to_string(),
                        "{name} :: {mode}: the two paths must fail the same way"
                    );
                    compared += 1;
                }
                (batch, streamed) => panic!(
                    "{name} :: {mode}: one path failed and the other did not — batch ok={}, stream ok={}",
                    batch.is_ok(),
                    streamed.is_ok()
                ),
            }
        }
    }
    assert!(
        compared >= 140,
        "only {compared} comparisons — the corpus shrank"
    );
}

/// [#55](TECH_DEBT.md): construction used to do the entire parse. On a
/// 5,000-page document that was over a second before the caller got anything
/// back, and the whole point of a stream is that it does not.
#[test]
fn construction_does_not_parse_the_document() {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "test_files",
        "pdf",
        "sample-5000-page.pdf",
    ]
    .iter()
    .collect();
    if !path.exists() {
        return;
    }
    let name = path.to_string_lossy().to_string();

    let started = Instant::now();
    let stream = pdf::stream(&name, "default", 3, 1, 3, 15).expect("construct");
    let construction = started.elapsed();

    // Reading a 5 MB file is all construction may do. The full parse is ~1.5 s,
    // so this bound is loose enough not to be flaky and tight enough to fail if
    // the work moves back.
    assert!(
        construction.as_millis() < 200,
        "construction took {construction:?} — it is parsing again"
    );

    // And the stream still delivers: the worker is doing the work meanwhile.
    assert!(stream.take(3).count() == 3, "no chunks arrived");
}

/// A consumer that stops early must not leave the worker wedged.
#[test]
fn abandoning_a_stream_early_is_clean() {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "test_files",
        "pdf",
        "sample-5000-page.pdf",
    ]
    .iter()
    .collect();
    if !path.exists() {
        return;
    }
    let name = path.to_string_lossy().to_string();
    let mut stream = pdf::stream(&name, "default", 3, 1, 3, 15).expect("construct");
    assert!(stream.next().is_some());
    drop(stream);
}

/// A failure reaches the caller whichever path it takes.
#[test]
fn a_text_less_pdf_reports_its_error_through_the_stream() {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "test_files",
        "pdf",
        "large-doc.pdf",
    ]
    .iter()
    .collect();
    if !path.exists() {
        return;
    }
    let name = path.to_string_lossy().to_string();
    let first = pdf::stream(&name, "default", 3, 1, 3, 15)
        .expect("construct")
        .next();
    match first {
        Some(Err(error)) => assert!(error.to_string().contains("no extractable text"), "{error}"),
        other => panic!("expected an error, got {:?}", other.map(|r| r.is_ok())),
    }
}

/// A path that is not a PDF is a caller error and is refused up front, since
/// nothing about it needs parsing to discover.
#[test]
fn a_non_pdf_path_is_refused_at_construction() {
    assert!(pdf::stream("notes.txt", "default", 3, 1, 3, 15).is_err());
}
