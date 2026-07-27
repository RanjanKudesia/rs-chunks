//! Fixture-driven CSV/TSV tests over the real corpus in `../test_files`.
//!
//! Runs every csv/tsv fixture through batch (all modes), streaming, and markdown,
//! asserting well-formed output and streaming/batch parity.

use std::path::{Path, PathBuf};

use chunks_rs::formats::csv;

fn fixtures_dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join(sub)
}

fn list(sub: &str, ext: &str) -> Vec<PathBuf> {
    let dir = fixtures_dir(sub);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext)) == Some(true) {
                out.push(p);
            }
        }
    }
    out.sort();
    assert!(!out.is_empty(), "no .{ext} fixtures found in {}", dir.display());
    out
}

fn well_formed(chunks: &[chunks_rs::Chunk]) {
    for c in chunks {
        assert!(!c.content_type.is_empty(), "empty content_type");
        assert!(c.metadata.is_object(), "metadata should be a JSON object");
    }
}

#[test]
fn csv_row_mode_all_fixtures() {
    for path in list("csv", "csv") {
        let p = path.to_str().unwrap();
        let chunks = csv::chunk(p, "row", 10, 5, 1, true, None, "utf-8", true)
            .unwrap_or_else(|e| panic!("row chunk failed for {p}: {e}"));
        assert!(!chunks.is_empty(), "no chunks from {p}");
        well_formed(&chunks);
        assert!(chunks.iter().all(|c| c.content_type == "row_group"));
    }
}

#[test]
fn csv_sliding_and_page_aware_modes() {
    for path in list("csv", "csv") {
        let p = path.to_str().unwrap();
        let sliding = csv::chunk(p, "sliding_window", 10, 5, 1, true, None, "utf-8", true)
            .unwrap_or_else(|e| panic!("sliding failed for {p}: {e}"));
        assert!(sliding.iter().all(|c| c.content_type == "row_window"));
        well_formed(&sliding);

        let page = csv::chunk(p, "page_aware", 15, 5, 1, true, None, "utf-8", true)
            .unwrap_or_else(|e| panic!("page_aware failed for {p}: {e}"));
        well_formed(&page);
    }
}

#[test]
fn tsv_row_mode_with_tab_delimiter() {
    for path in list("tsv", "tsv") {
        let p = path.to_str().unwrap();
        let chunks = csv::chunk(p, "row", 10, 5, 1, true, Some(b'\t'), "utf-8", true)
            .unwrap_or_else(|e| panic!("tsv row chunk failed for {p}: {e}"));
        assert!(!chunks.is_empty(), "no chunks from {p}");
        well_formed(&chunks);
    }
}

#[test]
fn streaming_matches_batch_row_mode() {
    for path in list("csv", "csv") {
        let p = path.to_str().unwrap();
        let batch = csv::chunk(p, "row", 10, 5, 1, true, None, "utf-8", true).unwrap();
        let streamed: Vec<_> = csv::stream(p, "row", 10, 5, 1, true, None, "utf-8", true)
            .unwrap_or_else(|e| panic!("stream failed for {p}: {e}"))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("stream item error for {p}: {e}"));
        assert_eq!(
            batch.len(),
            streamed.len(),
            "streaming/batch chunk count mismatch for {p}"
        );
        for (b, s) in batch.iter().zip(streamed.iter()) {
            assert_eq!(b.content, s.content, "content mismatch in {p}");
        }
    }
}

#[test]
fn markdown_produces_pipe_table() {
    for path in list("csv", "csv") {
        let p = path.to_str().unwrap();
        let md = csv::to_markdown(p, None, "utf-8")
            .unwrap_or_else(|e| panic!("to_markdown failed for {p}: {e}"));
        assert!(md.starts_with("| "), "markdown should start with a table row for {p}");
        assert!(md.contains("---"), "markdown should have a separator row for {p}");
    }
}

#[test]
fn invalid_args_fail_cleanly() {
    let p = list("csv", "csv")[0].to_str().unwrap().to_string();
    // overlap >= window_size
    assert!(csv::chunk(&p, "sliding_window", 10, 5, 5, true, None, "utf-8", true).is_err());
    // bad mode
    assert!(csv::chunk(&p, "nonsense", 10, 5, 1, true, None, "utf-8", true).is_err());
    // wrong extension
    assert!(csv::chunk("foo.pdf", "row", 10, 5, 1, true, None, "utf-8", true).is_err());
}
