//! `xlsx::stream` must agree with `xlsx::chunk`, mode for mode, chunk for chunk.
//!
//! `row` and `sliding_window` build their output through genuine state machines
//! that share no code with the batch builders — the batch path collects every
//! row of a sheet and slices it, the stream path walks a cursor and emits one
//! record at a time. Nothing but a direct comparison would catch them drifting,
//! and a drift here is invisible to the golden snapshot (which sweeps
//! `get_chunks` only) and to `parity_check.py` (which compares rs↔py batch).

use std::path::{Path, PathBuf};

use chunks_rs::chunk::Chunk;
use chunks_rs::error::ChunkError;
use chunks_rs::formats::xlsx;

const MODES: &[&str] = &[
    "row",
    "table",
    "sheet",
    "semantic",
    "page_aware",
    "sliding_window",
];

/// Select by *extension*, walking the whole corpus — the fixture directories
/// are not named after the extensions they hold (`.xlsx` lives under `excel/`,
/// the templates under `xltx_xltm/`). An earlier version of this test listed
/// directory names and silently swept a third of the corpus while still
/// passing its own "did we find enough files" assert.
const EXTS: &[&str] = &["xlsx", "xlsm", "xls", "xlsb", "ods", "xltx", "xltm"];

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
                && std::fs::metadata(&p).map(|m| m.len() < 8 * 1024 * 1024).unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("test_files"),
        &mut out,
    );
    out
}

fn signature(chunks: &[Chunk]) -> Vec<(String, String, String)> {
    chunks
        .iter()
        .map(|c| {
            (
                c.content.clone(),
                c.content_type.clone(),
                serde_json::to_string(&c.metadata).unwrap_or_default(),
            )
        })
        .collect()
}

/// TECH_DEBT #80: `build_sliding_window_chunks` lacks the header-row fallback
/// that `build_row_chunks` and the stream state machine both have, so a sheet
/// whose only row is its header is dropped by batch `sliding_window` and
/// emitted when streamed. 16 fixture/mode pairs in the corpus hit it.
///
/// The divergence predates this port — it reproduces in the shipped py_chunks
/// binding — so the migration preserves it rather than smuggling a behaviour
/// change into a refactor.
///
/// The exception is deliberately narrow: ignoring `chunk_index` (which shifts
/// downstream of any extra chunk, since the stream counts globally), the batch
/// output must be an order-preserving **subsequence** of the stream output. So
/// the stream may only ever emit *additional* chunks — any change to the
/// content or metadata of a shared chunk still fails the test.
fn is_known_sliding_window_header_only_gap(mode: &str, batch: &[Chunk], streamed: &[Chunk]) -> bool {
    if mode != "sliding_window" || streamed.len() <= batch.len() {
        return false;
    }
    let key = |c: &Chunk| {
        let mut meta = c.metadata.clone();
        if let Some(obj) = meta.as_object_mut() {
            obj.remove("chunk_index");
        }
        (c.content.clone(), c.content_type.clone(), meta.to_string())
    };
    let stream_keys: Vec<_> = streamed.iter().map(key).collect();
    let mut cursor = 0usize;
    for b in batch.iter().map(key) {
        match stream_keys[cursor..].iter().position(|s| *s == b) {
            Some(offset) => cursor += offset + 1,
            None => return false,
        }
    }
    true
}

/// The core guarantee. Content, `content_type` and **every metadata value** must
/// match — `skipped_sheets` and `chunk_index` are exactly the fields a
/// hand-written state machine gets wrong (#66 was a `skipped_sheets` mismatch).
#[test]
fn stream_matches_batch_for_every_mode() {
    let files = fixtures();
    assert!(
        files.len() >= 20,
        "expected the spreadsheet corpus, found {} files",
        files.len()
    );

    let mut compared = 0usize;
    let mut known_gaps = 0usize;
    for f in &files {
        let path = f.to_str().unwrap();
        for mode in MODES {
            let batch = xlsx::chunk(path, mode, 1, 3, 1, true, Vec::new(), true, 2000);
            let streamed = xlsx::stream(path, mode, 1, 3, 1, true, Vec::new(), true, 2000)
                .map(|s| s.collect::<Result<Vec<_>, _>>());

            match (batch, streamed) {
                (Ok(b), Ok(Ok(s))) => {
                    if is_known_sliding_window_header_only_gap(mode, &b, &s) {
                        known_gaps += 1;
                        continue;
                    }
                    assert_eq!(
                        signature(&b),
                        signature(&s),
                        "stream != batch for {} mode {mode}",
                        f.display()
                    );
                    compared += 1;
                }
                // Both must fail, and on the same kind of failure.
                (Err(be), Ok(Err(se))) | (Err(be), Err(se)) => assert_eq!(
                    std::mem::discriminant(&be),
                    std::mem::discriminant(&se),
                    "batch and stream disagree on error kind for {} mode {mode}: {be:?} vs {se:?}",
                    f.display()
                ),
                (Ok(b), Ok(Err(se))) => panic!(
                    "batch produced {} chunks but stream failed for {} mode {mode}: {se:?}",
                    b.len(),
                    f.display()
                ),
                (Ok(b), Err(se)) => panic!(
                    "batch produced {} chunks but stream failed to start for {} mode {mode}: {se:?}",
                    b.len(),
                    f.display()
                ),
                (Err(be), Ok(Ok(s))) => panic!(
                    "batch failed ({be:?}) but stream produced {} chunks for {} mode {mode}",
                    s.len(),
                    f.display()
                ),
            }
        }
    }
    assert!(compared > 0, "no fixture/mode pair compared successfully");
    // Pin the count. If #80 is fixed this drops to 0 and the assert fires,
    // which is the reminder to delete the exception rather than leave it
    // quietly permitting mismatches forever.
    assert_eq!(
        known_gaps, 16,
        "TECH_DEBT #80 affected 16 fixture/mode pairs; got {known_gaps}. \
         If the batch sliding_window header fallback landed, remove the exception."
    );
}

/// `row` and `sliding_window` must not build the whole result up front. Taking
/// one item from a many-chunk workbook and dropping the iterator is the
/// observable difference between a state machine and `chunk(...).into_iter()`.
#[test]
fn row_and_sliding_window_are_incremental() {
    let many = fixtures()
        .into_iter()
        .find(|f| {
            xlsx::chunk(f.to_str().unwrap(), "row", 1, 3, 1, true, Vec::new(), true, 2000)
                .map(|c| c.len() > 20)
                .unwrap_or(false)
        })
        .expect("a fixture with more than 20 row chunks");
    let path = many.to_str().unwrap();

    for mode in ["row", "sliding_window"] {
        let mut it = xlsx::stream(path, mode, 1, 3, 1, true, Vec::new(), true, 2000).unwrap();
        let first = it.next().expect("at least one chunk").unwrap();
        assert!(!first.content.is_empty(), "{mode} yielded an empty chunk");
        assert_eq!(
            first.metadata.get("chunk_index").and_then(|v| v.as_u64()),
            Some(0),
            "{mode} first chunk should be chunk_index 0"
        );
        // Drop mid-iteration: a state machine tolerates this, and nothing
        // downstream should have been computed yet.
        drop(it);
    }
}

/// The options the old signature hardcoded must actually reach the stream.
#[test]
fn stream_honours_the_options_the_old_signature_could_not_express() {
    let f = fixtures()
        .into_iter()
        .find(|f| {
            f.extension().and_then(|e| e.to_str()) == Some("xlsx")
                && xlsx::chunk(f.to_str().unwrap(), "row", 1, 3, 1, true, Vec::new(), true, 2000)
                    .map(|c| c.len() > 2)
                    .unwrap_or(false)
        })
        .expect("a multi-chunk .xlsx fixture");
    let path = f.to_str().unwrap();

    // include_headers = false changes the row serialisation.
    let with = xlsx::stream(path, "row", 1, 3, 1, true, Vec::new(), true, 2000)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let without = xlsx::stream(path, "row", 1, 3, 1, false, Vec::new(), true, 2000)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_ne!(
        with[0].content, without[0].content,
        "include_headers=false should change row content"
    );

    // rows_per_chunk is honoured (fewer chunks when grouping more rows).
    let grouped = xlsx::stream(path, "row", 5, 3, 1, true, Vec::new(), true, 2000)
        .unwrap()
        .count();
    assert!(
        grouped < with.len(),
        "rows_per_chunk=5 should yield fewer chunks than 1 ({grouped} vs {})",
        with.len()
    );

    // A named sheet that does not exist is a caller error, not a parse failure.
    let err = xlsx::stream(path, "row", 1, 3, 1, true, vec!["no such sheet".into()], true, 2000)
        .err()
        .expect("unknown sheet should fail");
    assert!(
        matches!(err, ChunkError::InvalidArg(ref m) if m.contains("not found")),
        "expected InvalidArg for a missing sheet, got {err:?}"
    );
}

/// A sheet name that is not in the workbook is a caller error on **both** paths.
///
/// `sheet_names` reaches the engine unvalidated from Python, so the exception
/// type is user-visible: py_chunks has always raised `ValueError` here and
/// `RuntimeError` for a genuine parse failure. The batch path used to lump both
/// into `Parse`; it now shares the stream's classifier.
#[test]
fn unknown_sheet_is_an_argument_error_on_both_paths() {
    let f = fixtures().into_iter().next().expect("a fixture");
    let path = f.to_str().unwrap();
    let missing = vec!["definitely not a sheet".to_string()];

    for mode in MODES {
        let batch = xlsx::chunk(path, mode, 1, 3, 1, true, missing.clone(), true, 2000);
        assert!(
            matches!(batch, Err(ChunkError::InvalidArg(ref m)) if m.contains("not found")),
            "batch {mode}: expected InvalidArg for an unknown sheet, got {batch:?}"
        );

        let streamed = xlsx::stream(path, mode, 1, 3, 1, true, missing.clone(), true, 2000)
            .err()
            .unwrap_or_else(|| panic!("stream {mode}: expected an error for an unknown sheet"));
        assert!(
            matches!(streamed, ChunkError::InvalidArg(ref m) if m.contains("not found")),
            "stream {mode}: expected InvalidArg for an unknown sheet, got {streamed:?}"
        );
    }
}

/// Argument validation matches the batch entry points.
#[test]
fn stream_rejects_the_same_arguments_batch_does() {
    let f = fixtures().into_iter().next().expect("a fixture");
    let path = f.to_str().unwrap();

    let bad = |mode: &str, rows: usize, win: usize, over: usize, max: usize| {
        xlsx::stream(path, mode, rows, win, over, true, Vec::new(), true, max).err()
    };

    assert!(matches!(bad("row", 0, 3, 1, 2000), Some(ChunkError::InvalidArg(m)) if m.contains("rows_per_chunk")));
    assert!(matches!(bad("semantic", 0, 3, 1, 2000), Some(ChunkError::InvalidArg(m)) if m.contains("rows_per_chunk")));
    assert!(matches!(bad("table", 1, 3, 1, 0), Some(ChunkError::InvalidArg(m)) if m.contains("max_chunk_chars")));
    assert!(matches!(bad("sheet", 1, 3, 1, 0), Some(ChunkError::InvalidArg(m)) if m.contains("max_chunk_chars")));
    assert!(matches!(bad("page_aware", 1, 3, 1, 0), Some(ChunkError::InvalidArg(m)) if m.contains("max_chunk_chars")));
    assert!(matches!(bad("sliding_window", 1, 0, 0, 2000), Some(ChunkError::InvalidArg(m)) if m.contains("window_size")));
    assert!(matches!(bad("sliding_window", 1, 2, 2, 2000), Some(ChunkError::InvalidArg(m)) if m.contains("overlap")));
    assert!(matches!(bad("nonsense", 1, 3, 1, 2000), Some(ChunkError::InvalidArg(m)) if m.contains("Unknown XLSX streaming mode")));
}

/// A password-protected workbook must say so.
///
/// calamine's content sniffing cannot classify an encrypted OOXML package and
/// reports "Cannot detect file format", losing the actionable reason. Opening
/// from a path never had this problem because calamine dispatched on the
/// extension; the engine opens from bytes, so it has to recover the specific
/// error itself. Regressing this turns a clear diagnostic into a shrug.
#[test]
fn password_protected_workbooks_report_the_real_reason() {
    let protected: Vec<PathBuf> = fixtures()
        .into_iter()
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains("protected") || n.contains("passtika"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        protected.len() >= 3,
        "expected the password-protected fixtures, found {}",
        protected.len()
    );

    for f in protected {
        let err = xlsx::chunk(f.to_str().unwrap(), "row", 1, 3, 1, true, Vec::new(), true, 2000)
            .err()
            .unwrap_or_else(|| panic!("{} should not open", f.display()));
        let msg = format!("{err:?}");
        assert!(
            msg.contains("password protected"),
            "{}: expected a password-protected diagnostic, got {msg}",
            f.display()
        );
    }
}
