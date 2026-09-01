//! Bad mode arguments must be `InvalidArg`, in every format.
//!
//! `docx`, `pptx`, `xlsx`, `csv`, `doc` and `ppt` always validated inline and
//! returned `InvalidArg`. The markdown-pipeline formats let their builders
//! return `Result<_, String>` and lifted the lot with `map_err(Parse)`, so
//! "overlap must be less than window_size" — raised before any parsing happens
//! — arrived as a *parse failure*. js-chunks reported `kind: "parse"` there and
//! `"invalid-arg"` for csv/xlsx; py-chunks hid the split behind its own
//! Python-layer `ValueError`.
//!
//! `legacy_binary_args.rs` pins the same contract for `.doc`/`.ppt`.

use chunks_rs::ChunkError;

#[track_caller]
fn assert_invalid_arg<T: std::fmt::Debug>(
    got: chunks_rs::Result<T>,
    want_message: &str,
    what: &str,
) {
    match got {
        Err(ChunkError::InvalidArg(m)) => assert_eq!(m, want_message, "{what}: wrong message"),
        other => panic!("{what}: expected InvalidArg({want_message:?}), got {other:?}"),
    }
}

const MD: &[u8] = b"# Title\n\nA paragraph of prose.\n\nAnother one.\n";
const TXT: &[u8] = b"A paragraph of prose.\n\nAnother one.\n";
const HTML: &[u8] = b"<html><body><p>A paragraph.</p><p>Another.</p></body></html>";

#[test]
fn markdown_pipeline_formats_reject_bad_window_args_as_invalid_arg() {
    for (name, bytes, chunk) in [
        (
            "md",
            MD,
            chunks_rs::formats::md::chunk_from_bytes
                as fn(
                    &[u8],
                    &str,
                    usize,
                    usize,
                    usize,
                    usize,
                ) -> chunks_rs::Result<Vec<chunks_rs::Chunk>>,
        ),
        ("txt", TXT, chunks_rs::formats::txt::chunk_from_bytes),
        ("html", HTML, chunks_rs::formats::html::chunk_from_bytes),
    ] {
        assert_invalid_arg(
            chunk(bytes, "sliding_window", 3, 3, 3, 15),
            "overlap must be less than window_size",
            name,
        );
        assert_invalid_arg(
            chunk(bytes, "sliding_window", 0, 0, 3, 15),
            "window_size must be greater than 0",
            name,
        );
        assert_invalid_arg(
            chunk(bytes, "sentence", 3, 1, 0, 15),
            "sentences_per_chunk must be greater than 0",
            name,
        );
        assert_invalid_arg(
            chunk(bytes, "page_aware", 3, 1, 3, 0),
            "paragraphs_per_page must be greater than 0",
            name,
        );
    }
}

/// The pipeline formats (odf, eml, json, rtf, msg, ipynb, pdf) reach the same
/// guard through `md::build_records_from_bytes`, so one of them standing in for
/// the family is enough to prove the route.
///
/// **`epub` is NOT one of them** — it drives the HTML builders directly and has
/// its own facade guard. It is covered by `epub_validates_its_mode_arguments`
/// below; this entry used to claim epub, which is how the gap survived.
#[test]
fn pipeline_formats_inherit_the_same_variant() {
    let json = br#"[{"a": 1}, {"a": 2}]"#;
    assert_invalid_arg(
        chunks_rs::formats::json::chunk_from_bytes(json, "x.json", "sliding_window", 3, 3, 3, 15),
        "overlap must be less than window_size",
        "json",
    );
}

/// The formats that already got this right must keep getting it right.
#[test]
fn inline_validating_formats_are_unchanged() {
    let csv = b"a,b\n1,2\n3,4\n";
    assert_invalid_arg(
        chunks_rs::formats::csv::chunk_from_bytes(
            csv,
            "sliding_window",
            3,    // rows_per_chunk
            2,    // window_size
            2,    // overlap
            true, // include_headers
            None, // delimiter
            "utf-8",
            true, // skip_empty_rows
        ),
        "overlap must be less than window_size",
        "csv",
    );
}

// ── EPUB: the format that validated nowhere ─────────────────────────────────
//
// EPUB drives the HTML builders directly instead of routing through the
// markdown pipeline, so it never picked up the shared guard — and
// `epub/extract.rs::chunk_package` swallows per-spine-document builder failures
// on purpose (an image-only cover page must not abort a whole book), so a bad
// argument produced an EMPTY CHUNK LIST instead of an error.

const EPUB_MODE_MESSAGE: &str = "mode must be one of [\"default\", \"structural\", \"section\", \"semantic\", \"sentence\", \"page_aware\", \"sliding_window\"] for EPUB, got: 'nope'";

/// Bytes that are not an EPUB at all: validation runs *before* the parse, so a
/// rejected argument must still be the error reported — proof the guard is not
/// hiding behind a successful parse.
#[test]
fn epub_validates_its_mode_arguments() {
    let junk = b"not an epub";
    for (mode, ws, ov, spc, ppp, want) in [
        (
            "sliding_window",
            100usize,
            100usize,
            3usize,
            15usize,
            "overlap must be less than window_size",
        ),
        (
            "sliding_window",
            0,
            0,
            3,
            15,
            "window_size must be greater than 0",
        ),
        (
            "sentence",
            3,
            1,
            0,
            15,
            "sentences_per_chunk must be greater than 0",
        ),
        (
            "page_aware",
            3,
            1,
            3,
            0,
            "paragraphs_per_page must be greater than 0",
        ),
    ] {
        assert_invalid_arg(
            chunks_rs::formats::epub::chunk_from_bytes(junk, mode, ws, ov, spc, ppp),
            want,
            &format!("epub bytes {mode}"),
        );
        assert_invalid_arg(
            chunks_rs::formats::epub::chunk_with_images_from_bytes(junk, mode, ws, ov, spc, ppp),
            want,
            &format!("epub bytes+images {mode}"),
        );
    }
    assert_invalid_arg(
        chunks_rs::formats::epub::chunk_from_bytes(junk, "nope", 3, 1, 3, 15),
        EPUB_MODE_MESSAGE,
        "epub unknown mode",
    );
}

/// The path route, on a real book — so "returns an empty array" is pinned
/// against input that demonstrably does produce chunks.
#[test]
fn epub_path_route_rejects_bad_args_on_a_real_book() {
    let book = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join("epub")
        .join("gutenberg_moby_dick.epub");
    if !book.is_file() {
        eprintln!("skipping: {} not present", book.display());
        return;
    }
    let path = book.to_str().expect("utf-8 fixture path");

    // The same call with a *usable* overlap returns a real book. That contrast
    // is the point: the bug was that the invalid call returned `Ok(vec![])`,
    // indistinguishable from "this book has no content".
    let valid = chunks_rs::formats::epub::chunk(path, "sliding_window", 100, 20, 3, 15)
        .expect("a valid sliding_window call must chunk the book");
    assert!(
        valid.len() > 10,
        "sanity: expected a real book, got {} chunks",
        valid.len()
    );
    let default_mode = chunks_rs::formats::epub::chunk(path, "default", 3, 1, 3, 15)
        .expect("default mode must chunk the book");
    assert!(
        default_mode.len() > 1000,
        "sanity: got {} chunks",
        default_mode.len()
    );

    assert_invalid_arg(
        chunks_rs::formats::epub::chunk(path, "sliding_window", 100, 100, 3, 15),
        "overlap must be less than window_size",
        "epub path",
    );
    assert_invalid_arg(
        chunks_rs::formats::epub::chunk_with_images(path, "page_aware", 3, 1, 3, 0),
        "paragraphs_per_page must be greater than 0",
        "epub path+images",
    );
    // The dispatch entry point too — that is what every SDK actually calls.
    assert_invalid_arg(
        chunks_rs::get_chunks(path, "sliding_window", 100, 100, 3, 15),
        "overlap must be less than window_size",
        "epub via get_chunks",
    );
}

// ── Spreadsheets: a parameter that was dropped, not validated ───────────────

/// Spreadsheets paginate by `rows_per_chunk`, so the dispatch arms drop
/// `paragraphs_per_page` — which is why `page_aware` with 0 was accepted here
/// while every other format rejects it.
#[test]
fn spreadsheets_reject_paragraphs_per_page_zero() {
    let junk = b"not a spreadsheet";
    assert_invalid_arg(
        chunks_rs::get_chunks_from_bytes(junk, "x.xlsx", "page_aware", 3, 1, 3, 0),
        "paragraphs_per_page must be greater than 0",
        "xlsx bytes",
    );
    assert_invalid_arg(
        chunks_rs::get_chunks_with_images_from_bytes(junk, "x.xlsx", "page_aware", 3, 1, 3, 0),
        "paragraphs_per_page must be greater than 0",
        "xlsx bytes+images",
    );
    assert_invalid_arg(
        chunks_rs::formats::xlsx::chunk_with_options(
            "x.xlsx",
            &chunks_rs::ChunkOptions {
                mode: chunks_rs::ChunkMode::PageAware,
                paragraphs_per_page: 0,
                ..Default::default()
            },
        ),
        "paragraphs_per_page must be greater than 0",
        "xlsx chunk_with_options",
    );
}

/// Mode-scoped exactly like `options::validate_mode_args`: `row` never reads
/// `paragraphs_per_page`, so 0 there is not a caller mistake — and is not
/// rejected for any other format either. (The parse still fails on junk bytes;
/// what matters is that it is *not* an `InvalidArg` about the page argument.)
#[test]
fn spreadsheet_page_arg_check_is_mode_scoped() {
    let junk = b"not a spreadsheet";
    if let Err(ChunkError::InvalidArg(m)) =
        chunks_rs::get_chunks_from_bytes(junk, "x.xlsx", "row", 3, 1, 3, 0)
    {
        panic!("row mode must not reject paragraphs_per_page=0, got InvalidArg({m:?})")
    }
}

/// The spreadsheet family was the last holder of `window_size must be >= 1`;
/// every format now uses the canonical sentence.
#[test]
fn spreadsheet_window_size_message_is_canonical() {
    let junk = b"not a spreadsheet";
    assert_invalid_arg(
        chunks_rs::formats::xlsx::chunk_from_bytes(
            junk,
            "xlsx",
            "sliding_window",
            1,    // rows_per_chunk
            0,    // window_size
            0,    // overlap
            true, // include_headers
            Vec::new(),
            true, // skip_empty_rows
            2000, // max_chunk_chars
        ),
        "window_size must be greater than 0",
        "xlsx sliding_window from bytes",
    );
    // `chunk_with_images_from_bytes` opens the workbook first (it needs the
    // sheet names before it can attribute images), so junk bytes fail as a
    // parse error there and would hide the message under test. Drive it with a
    // real fixture instead — the guard itself is a separate string, so it needs
    // its own coverage.
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join("excel");
    let real = std::fs::read_dir(&fixture).ok().and_then(|entries| {
        entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("xlsx"))
            .find(|p| p.metadata().map(|m| m.len() < 2_000_000).unwrap_or(false))
    });
    let Some(real) = real else {
        eprintln!("skipping the with-images half: no small .xlsx fixture found");
        return;
    };
    let data = std::fs::read(&real).expect("read xlsx fixture");
    assert_invalid_arg(
        chunks_rs::formats::xlsx::chunk_with_images_from_bytes(
            &data,
            "xlsx",
            "sliding_window",
            1,
            0,
            0,
            true,
            Vec::new(),
            true,
            2000,
        ),
        "window_size must be greater than 0",
        "xlsx sliding_window with images",
    );
}
