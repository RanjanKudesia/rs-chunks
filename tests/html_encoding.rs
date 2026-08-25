//! Non-UTF-8 HTML must decode rather than fail or mangle (TECH_DEBT C7).
//!
//! Found by the neutral x86 full-corpus run: these were 2 of only 3 legitimate
//! documents in 638 that the engine could not read, and both competitors read
//! them. The 64-file sample never caught it, because that sample takes the two
//! *smallest* files per directory.
//!
//! The engine used to read HTML three different ways — `fs::read_to_string`
//! (hard error) at two entry points and `String::from_utf8_lossy` (silent
//! mojibake) at six others — so the same document behaved differently depending
//! on which function you called. These tests pin both halves: it decodes, and
//! every entry point agrees.

use std::path::{Path, PathBuf};

/// Asserts rather than returning `Option`. This used to be
/// `Option<PathBuf>` with every test opening `let Some(corpus) = corpus() else
/// { return }`, and every per-file guard was a silent `continue`/`return` too —
/// so deleting the fixtures left all four tests **passing**. A test that pins
/// nothing while appearing to pin C7 is worse than no test, because the gap it
/// guards is then believed closed.
fn corpus() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .join("test_files");
    assert!(
        dir.is_dir(),
        "corpus missing at {} — this test is not optional",
        dir.display()
    );
    dir
}

/// A fixture named by a test is required, not best-effort.
fn required(dir: &Path, rel: &str) -> PathBuf {
    let p = dir.join(rel);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

/// Files whose declared encoding is not UTF-8, with the label they declare.
const DECLARED: &[(&str, &str)] = &[
    ("html/tika_big-preamble.html", "windows-1251"),
    ("html/tika_noisy-meta-encoding-arabic.html", "iso-8859-6"),
];

#[test]
fn non_utf8_html_decodes_without_replacement_chars() {
    let corpus = corpus();
    for (rel, label) in DECLARED {
        let path = required(&corpus, rel);
        let p = path.to_str().unwrap();

        let md = chunks_rs::formats::html::to_markdown(p)
            .unwrap_or_else(|e| panic!("{rel} (declares {label}) failed to_markdown: {e}"));
        assert!(!md.is_empty(), "{rel}: empty markdown");
        assert_eq!(
            md.matches('\u{FFFD}').count(),
            0,
            "{rel}: decoded with replacement characters — the declared {label} was ignored"
        );

        let chunks = chunks_rs::get_chunks(p, "default", 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("{rel} failed get_chunks: {e}"));
        assert!(!chunks.is_empty(), "{rel}: no chunks");
        for c in &chunks {
            assert_eq!(
                c.content.matches('\u{FFFD}').count(),
                0,
                "{rel}: chunk content has replacement characters"
            );
        }
    }
}

/// The split-brain half: path and bytes entry points must agree.
///
/// `to_markdown` read a `String` and `to_markdown_from_bytes` read bytes
/// lossily, so on a non-UTF-8 document one errored while the other returned
/// mojibake.
#[test]
fn path_and_bytes_entry_points_agree() {
    let corpus = corpus();
    for (rel, _) in DECLARED {
        let path = required(&corpus, rel);
        let p = path.to_str().unwrap();
        let bytes = std::fs::read(&path).unwrap();

        let from_path = chunks_rs::formats::html::to_markdown(p).unwrap();
        let from_bytes = chunks_rs::formats::html::to_markdown_from_bytes(&bytes).unwrap();
        assert_eq!(
            from_path, from_bytes,
            "{rel}: path and bytes markdown differ"
        );

        let chunks_path = chunks_rs::get_chunks(p, "default", 3, 1, 3, 15).unwrap();
        let chunks_bytes =
            chunks_rs::formats::html::chunk_from_bytes(&bytes, "default", 3, 1, 3, 15).unwrap();
        assert_eq!(
            chunks_path.len(),
            chunks_bytes.len(),
            "{rel}: path and bytes chunk counts differ"
        );
    }
}

/// Every mode must decode the same way — the bug was that each strategy file
/// carried its own copy of the decode expression.
#[test]
fn every_mode_decodes_the_same_document() {
    let corpus = corpus();
    let path = required(&corpus, "html/tika_big-preamble.html");
    let p = path.to_str().unwrap();
    for mode in [
        "default",
        "structural",
        "section",
        "semantic",
        "sentence",
        "page_aware",
        "sliding_window",
    ] {
        let chunks = chunks_rs::get_chunks(p, mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("mode {mode} failed: {e}"));
        assert!(!chunks.is_empty(), "mode {mode}: no chunks");
        for c in &chunks {
            assert_eq!(
                c.content.matches('\u{FFFD}').count(),
                0,
                "mode {mode}: replacement characters in chunk content"
            );
        }
    }
}

/// A plain UTF-8 document must be byte-identical to what it was before C7.
///
/// This is what makes the change safe: the decoder only consults a declaration
/// or detection for bytes that are *not* valid UTF-8.
#[test]
fn utf8_html_is_unaffected() {
    let corpus = corpus();
    let dir = corpus.join("html");
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("html corpus unreadable at {}: {e}", dir.display()));
    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if std::str::from_utf8(&bytes).is_err() {
            continue; // the non-UTF-8 files are covered above
        }
        if let Ok(md) = chunks_rs::formats::html::to_markdown(path.to_str().unwrap()) {
            assert_eq!(
                md.matches('\u{FFFD}').count(),
                0,
                "{:?}: valid UTF-8 gained replacement characters",
                path.file_name().unwrap()
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no UTF-8 html fixtures exercised");
}

/// BOM-less UTF-16 must decode too.
///
/// `decode_html` checked valid-UTF-8 before sniffing UTF-16, which
/// `text_encoding::decode_raw` documents as the wrong order: ASCII in UTF-16LE
/// is `"T\0h\0e\0"`, and NUL is a valid UTF-8 codepoint, so `from_utf8`
/// accepts it and returns the NUL-interleaved bytes. Measured before the fix,
/// the page came back with its tags unparsed — while the identical bytes named
/// `.txt` decoded correctly.
#[test]
fn bomless_utf16_html_decodes() {
    let doc = "<html><body><h1>Heading One</h1><p>Body text here.</p></body></html>";
    for (label, bytes) in [
        (
            "utf-16le",
            doc.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>(),
        ),
        (
            "utf-16be",
            doc.encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<u8>>(),
        ),
    ] {
        let md = chunks_rs::formats::html::to_markdown_from_bytes(&bytes)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(
            !md.contains('\0'),
            "{label}: returned NUL-interleaved bytes: {:?}",
            &md[..md.len().min(40)]
        );
        assert!(
            md.contains("Heading One"),
            "{label}: text not decoded: {md:?}"
        );
        let chunks = chunks_rs::formats::html::chunk_from_bytes(&bytes, "structural", 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("{label} chunks: {e}"));
        assert!(
            chunks.iter().any(|c| c.content_type == "heading"),
            "{label}: markup was not parsed, so no heading was found: {chunks:?}"
        );
    }
}
