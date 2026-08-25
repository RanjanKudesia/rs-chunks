//! `get_markdown` and `get_chunks` must decode a document identically.
//!
//! They did not. On the same cp1252 bytes `md::to_markdown` returned a hard
//! error — "MD not valid UTF-8" — while the six chunk strategies decoded
//! lossily and returned U+FFFD. One document, two answers, and neither of them
//! the text. The identical bytes through `.html` came back clean, because C7
//! had been fixed there and never propagated next door.
//!
//! Markdown carries no in-band encoding declaration, so unlike HTML there is
//! nothing extra to consult: it is exactly the `.txt` ladder.

use std::path::{Path, PathBuf};

use chunks_rs::formats::md;

/// Panics rather than skipping. `tests/html_encoding.rs` guards every case with
/// `let Some(corpus) = corpus() else { return }` and per-file `continue`s, so
/// deleting its fixtures leaves all four tests passing — it pins nothing while
/// appearing to pin C7. This must fail loudly instead.
fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .join("test_files/md")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

const MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

#[test]
fn to_markdown_and_get_chunks_agree_on_cp1252() {
    let p = fixture("derived_cp1252.md");
    let path = p.to_str().unwrap();

    let markdown = md::to_markdown(path).expect("cp1252 markdown must decode, not error");
    assert!(
        !markdown.contains('\u{FFFD}'),
        "to_markdown produced replacement chars: {markdown:?}"
    );
    for needle in ["Café", "naïve", "\u{201C}quotes\u{201D}", "\u{2014}"] {
        assert!(
            markdown.contains(needle),
            "to_markdown lost {needle:?}: {markdown:?}"
        );
    }

    for mode in MODES {
        let chunks = md::chunk(path, mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("cp1252 [{mode}] must decode, got {e}"));
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(
            !joined.contains('\u{FFFD}'),
            "[{mode}] produced replacement chars: {joined:?}"
        );
        assert!(
            joined.contains("Café"),
            "[{mode}] lost the decoded text: {joined:?}"
        );
    }
}

#[test]
fn path_and_bytes_entry_points_agree() {
    for name in ["derived_cp1252.md", "derived_utf8bom.md"] {
        let p = fixture(name);
        let bytes = std::fs::read(&p).unwrap();
        let via_path = md::to_markdown(p.to_str().unwrap()).expect("path entry point");
        let via_bytes = md::to_markdown_from_bytes(&bytes).expect("bytes entry point");
        assert_eq!(via_path, via_bytes, "{name}: entry points disagree");
    }
}

/// A leading U+FEFF is a signature, not content. Left in place it prefixed the
/// first line, so `\u{feff}# Heading` failed `line.starts_with('#')` and was
/// classified as an ordinary paragraph — the heading was then merged into the
/// following block. A BOM'd markdown file silently lost its entire heading
/// structure, and `structural` and `section` degraded to flat text.
#[test]
fn a_bom_does_not_destroy_the_heading_structure() {
    let bom = fixture("derived_utf8bom.md");
    let markdown = md::to_markdown(bom.to_str().unwrap()).expect("bom markdown");
    assert!(
        !markdown.starts_with('\u{FEFF}'),
        "the BOM survived into the markdown: {:?}",
        &markdown[..markdown.len().min(20)]
    );

    for mode in ["structural", "section"] {
        let chunks = md::chunk(bom.to_str().unwrap(), mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("bom [{mode}]: {e}"));
        assert!(
            chunks.iter().any(|c| c.content_type == "heading"),
            "[{mode}] found no heading at all: {:?}",
            chunks
                .iter()
                .map(|c| (&c.content_type, &c.content))
                .collect::<Vec<_>>()
        );
    }
}

/// The BOM must be the *only* difference: same document, same chunks.
#[test]
fn bom_and_bomless_produce_identical_chunks() {
    let bom = std::fs::read(fixture("derived_utf8bom.md")).unwrap();
    let bomless = bom.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap().to_vec();

    for mode in MODES {
        let a = md::chunk_from_bytes(&bom, mode, 3, 1, 3, 15).unwrap();
        let b = md::chunk_from_bytes(&bomless, mode, 3, 1, 3, 15).unwrap();
        let ta: Vec<&str> = a.iter().map(|c| c.content.as_str()).collect();
        let tb: Vec<&str> = b.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(ta, tb, "[{mode}] a BOM changed the chunks");
    }
}

/// The safety half: every real fixture must be untouched by the decoder swap.
#[test]
fn valid_utf8_markdown_is_unaffected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/md");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("md corpus must exist") {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if p.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("derived_")
        {
            continue;
        }
        let md_text =
            md::to_markdown(p.to_str().unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(
            !md_text.contains('\u{FFFD}'),
            "{}: replacement chars in a valid UTF-8 fixture",
            p.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no .md fixtures exercised — the corpus is missing"
    );
}
