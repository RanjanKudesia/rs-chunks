//! Spreadsheet `semantic` chunks must respect the documented 1,500-character cap.
//!
//! Regression test for TECH_DEBT T1. `formats/xlsx/semantic.rs` grouped rows by
//! detected category and emitted **one chunk per group with no upper bound at
//! all** — every other semantic chunker (txt, html, pptx, docx) applies
//! `MAX_SEMANTIC_CHARS`, and this one did not. An external 335-file sweep found
//! a 224,718-character chunk (~56k tokens) on
//! `xlsm/mv-calculator-final-2-20-2013.xlsm`, larger than the context window of
//! every common embedding model, on the mode the docs recommend as the starting
//! point.
//!
//! The bound is asserted as a **corpus-wide invariant** rather than against that
//! one fixture, which is not in this corpus: any spreadsheet, any category
//! distribution, the contract holds.
//!
//! The cap is duplicated here on purpose. `shared::MAX_SEMANTIC_CHARS` is
//! `pub(crate)` so an integration test cannot import it — and
//! `chunking-modes/semantic.mdx` publishes **1,500** as a fixed number, so if
//! the constant ever moves this test must fail and force the docs to move too.

use std::path::{Path, PathBuf};

use chunks_rs::formats::xlsx;

/// Mirrors `shared::MAX_SEMANTIC_CHARS` and the published docs figure.
const MAX_SEMANTIC_CHARS: usize = 1500;

const EXTS: &[&str] = &["xlsx", "xlsm", "xls", "xlsb", "ods", "xltx", "xltm"];

/// Select by extension, walking the whole corpus — fixture directories are not
/// named after the extensions they hold (`.xlsx` lives under `excel/`). Same
/// walker as `xlsx_stream.rs`, for the same reason.
fn fixtures() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() {
                walk(&p, out);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
                && std::fs::metadata(&p)
                    .map(|m| m.len() < 8 * 1024 * 1024)
                    .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("test_files"),
        &mut out,
    );
    out
}

#[test]
fn semantic_chunks_respect_the_documented_cap() {
    let files = fixtures();
    if files.is_empty() {
        eprintln!("test_files corpus absent — skipping (see release.yml)");
        return;
    }

    let mut violations: Vec<String> = Vec::new();
    let mut multi_row_chunks = 0usize;
    let mut total_chunks = 0usize;
    let mut largest_multi_row = 0usize;
    let mut largest_atomic = 0usize;
    let mut files_chunked = 0usize;

    for path in &files {
        // `rows_per_chunk = 1` is what `get_chunks` passes for spreadsheets
        // (the `sentences_per_chunk == 3` sentinel, see dispatch.rs).
        let Ok(chunks) = xlsx::chunk(
            &path.to_string_lossy(),
            "semantic",
            1,
            1,
            0,
            true,
            Vec::new(),
            true,
            2000,
        ) else {
            // Unreadable fixtures are covered by the adversarial suite, not here.
            continue;
        };
        files_chunked += 1;

        for chunk in &chunks {
            total_chunks += 1;
            let chars = chunk.content.chars().count();
            let rows = chunk
                .metadata
                .get("actual_row_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(1);

            if rows > 1 {
                multi_row_chunks += 1;
                largest_multi_row = largest_multi_row.max(chars);
                if chars > MAX_SEMANTIC_CHARS {
                    violations.push(format!(
                        "{}: {} chars across {} rows (cap {})",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        chars,
                        rows,
                        MAX_SEMANTIC_CHARS
                    ));
                }
            } else {
                // A single row wider than the cap is an indivisible unit and is
                // allowed to exceed it, exactly as in every other semantic
                // chunker. Splitting mid-row would corrupt the record.
                largest_atomic = largest_atomic.max(chars);
            }
        }
    }

    eprintln!(
        "swept {files_chunked}/{} fixtures, {total_chunks} chunks \
         ({multi_row_chunks} multi-row); largest multi-row {largest_multi_row} chars, \
         largest atomic row {largest_atomic} chars",
        files.len()
    );

    assert!(
        violations.is_empty(),
        "semantic chunks exceeded the documented {MAX_SEMANTIC_CHARS}-char cap \
         without being a single indivisible row:\n{}",
        violations.join("\n")
    );

    // Guard against the assertion above passing vacuously: if the corpus stops
    // producing grouped chunks, this test proves nothing and must be revisited.
    assert!(
        multi_row_chunks > 0,
        "no multi-row semantic chunks were produced across {files_chunked} \
         spreadsheet fixtures — the cap assertion would be vacuous"
    );
}
