//! `.doc` / `.ppt` per-mode argument validation.
//!
//! The paragraph builders degrade to an empty `Vec` when `window_size` is 0 or
//! `overlap >= window_size`, which turns a caller mistake into a document that
//! silently "has no content". These entry points reject it instead.
//!
//! The fixtures here are deliberately garbage bytes: validation must happen
//! *before* parsing, so a bad argument reports the bad argument rather than a
//! parse failure. That ordering is the part most likely to regress.

use chunks_rs::error::ChunkError;

fn assert_invalid_arg(res: Result<impl Sized, ChunkError>, expected: &str) {
    match res {
        Err(ChunkError::InvalidArg(m)) => assert_eq!(m, expected),
        Err(other) => panic!("expected InvalidArg({expected:?}), got {other:?}"),
        Ok(_) => panic!("expected InvalidArg({expected:?}), got Ok"),
    }
}

/// A file with the right extension whose contents are not a valid CFB
/// container: parsing it must never be reached.
fn unparseable(ext: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("chunks_rs_arg_validation.{ext}"));
    std::fs::write(&path, b"not a compound file").unwrap();
    path
}

#[test]
fn doc_rejects_bad_mode_args_before_parsing() {
    let p = unparseable("doc");
    let f = p.to_str().unwrap();

    assert_invalid_arg(
        chunks_rs::formats::doc::chunk(f, "sliding_window", 0, 0, 3, 15),
        "window_size must be greater than 0",
    );
    assert_invalid_arg(
        chunks_rs::formats::doc::chunk(f, "sliding_window", 2, 2, 3, 15),
        "overlap must be less than window_size",
    );
    assert_invalid_arg(
        chunks_rs::formats::doc::chunk(f, "sentence", 3, 1, 0, 15),
        "sentences_per_chunk must be greater than 0",
    );
    assert_invalid_arg(
        chunks_rs::formats::doc::chunk(f, "page_aware", 3, 1, 3, 0),
        "paragraphs_per_page must be greater than 0",
    );
}

#[test]
fn ppt_rejects_bad_mode_args_before_parsing() {
    let p = unparseable("ppt");
    let f = p.to_str().unwrap();

    assert_invalid_arg(
        chunks_rs::formats::ppt::chunk(f, "sliding_window", 0, 0, 3, 15),
        "window_size must be greater than 0",
    );
    assert_invalid_arg(
        chunks_rs::formats::ppt::chunk(f, "sliding_window", 2, 2, 3, 15),
        "overlap must be less than window_size",
    );
    assert_invalid_arg(
        chunks_rs::formats::ppt::chunk(f, "sentence", 3, 1, 0, 15),
        "sentences_per_chunk must be greater than 0",
    );
    assert_invalid_arg(
        chunks_rs::formats::ppt::chunk(f, "page_aware", 3, 1, 3, 0),
        "paragraphs_per_page must be greater than 0",
    );
}

/// The no-filesystem entry points (wasm/browser) share the same guard.
#[test]
fn from_bytes_entry_points_validate_too() {
    let junk = b"not a compound file";
    assert_invalid_arg(
        chunks_rs::formats::doc::chunk_from_bytes(junk, "a.doc", "sliding_window", 2, 2, 3, 15),
        "overlap must be less than window_size",
    );
    assert_invalid_arg(
        chunks_rs::formats::ppt::chunk_from_bytes(junk, "a.ppt", "page_aware", 3, 1, 3, 0),
        "paragraphs_per_page must be greater than 0",
    );
    assert_invalid_arg(
        chunks_rs::formats::doc::chunk_with_images_from_bytes(
            junk, "a.doc", "sentence", 3, 1, 0, 15,
        ),
        "sentences_per_chunk must be greater than 0",
    );
    assert_invalid_arg(
        chunks_rs::formats::ppt::chunk_with_images_from_bytes(
            junk,
            "a.ppt",
            "sliding_window",
            0,
            0,
            3,
            15,
        ),
        "window_size must be greater than 0",
    );
}

/// Modes that do not take a parameter must not be affected by its value: a
/// `structural` call with `window_size = 0` is legitimate.
#[test]
fn unrelated_modes_ignore_the_arguments() {
    let p = unparseable("doc");
    let f = p.to_str().unwrap();
    // Reaches the parser and fails there — which is the point: not InvalidArg.
    assert!(matches!(
        chunks_rs::formats::doc::chunk(f, "structural", 0, 0, 0, 0),
        Err(ChunkError::Parse(_))
    ));
}
