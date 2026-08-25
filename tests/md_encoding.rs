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

/// YAML front matter is metadata for a site generator, not body text.
///
/// Leaving it in did two things: it became chunk 1 verbatim, so every
/// Fumadocs/Hugo page opened with `title: …`; and the closing `---` read as a
/// **setext underline**, promoting the last front-matter line to a heading.
/// Measured before the fix, `tags: [a, b]` was classified `heading` — a page's
/// section structure began with a YAML key.
#[test]
fn yaml_front_matter_is_not_body_text() {
    let doc = "---\ntitle: My Post\ndraft: false\ntags: [a, b]\n---\n\n               # Real Heading\n\nBody text long enough to be a real paragraph.\n";
    for mode in ["structural", "section"] {
        let chunks = md::chunk_from_bytes(doc.as_bytes(), mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("[{mode}]: {e}"));
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(!joined.contains("draft: false"), "[{mode}] front matter leaked: {joined:?}");
        assert!(!joined.contains("tags:"), "[{mode}] front matter leaked: {joined:?}");
        assert!(joined.contains("Real Heading"), "[{mode}] lost the real heading: {joined:?}");

        let first_heading = chunks.iter().find(|c| c.content_type == "heading");
        assert!(
            first_heading.is_some_and(|c| c.content.contains("Real Heading")),
            "[{mode}] the first heading is not the document's: {:?}",
            first_heading.map(|c| &c.content)
        );
    }
}

/// A `---` that is not a leading fence must keep its meaning: a horizontal rule
/// or a setext underline. Only a block at the very start is front matter.
#[test]
fn a_later_rule_is_not_treated_as_front_matter() {
    let doc = "# Title\n\nFirst paragraph with enough words in it.\n\n---\n\n               Second paragraph with enough words in it too.\n";
    let chunks = md::chunk_from_bytes(doc.as_bytes(), "structural", 3, 1, 3, 15).unwrap();
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(joined.contains("First paragraph"), "lost content: {joined:?}");
    assert!(joined.contains("Second paragraph"), "lost content: {joined:?}");
}

/// An unterminated fence is not front matter and must not swallow the document.
#[test]
fn an_unterminated_fence_is_left_alone() {
    let doc = "---\ntitle: never closed\n\nBody text that must survive intact here.\n";
    let chunks = md::chunk_from_bytes(doc.as_bytes(), "structural", 3, 1, 3, 15).unwrap();
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(
        joined.contains("Body text that must survive"),
        "an unterminated fence swallowed the document: {joined:?}"
    );
}

/// CommonMark 6.1: only ASCII **punctuation** may be backslash-escaped.
/// "Backslashes before other characters are treated as literal backslashes."
///
/// `strip_inline` dropped the backslash before any character at all, so an
/// ordinary Windows path was rewritten: `C:\new\dir` -> `C:newdir`. The escape
/// rule was quietly editing prose.
#[test]
fn a_backslash_before_a_letter_is_literal() {
    let doc = "A path like C:\\new\\dir must keep its separators in this line.\n";
    let chunks = md::chunk_from_bytes(doc.as_bytes(), "structural", 3, 1, 3, 15).unwrap();
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(
        joined.contains(r"C:\new\dir"),
        "the path lost its backslashes: {joined:?}"
    );
}

/// A real escape must still work — this is the half that must not regress.
#[test]
fn a_backslash_before_punctuation_still_escapes() {
    let doc = "A literal asterisk \\* and a literal underscore \\_ in this line here.\n";
    let chunks = md::chunk_from_bytes(doc.as_bytes(), "structural", 3, 1, 3, 15).unwrap();
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(joined.contains("asterisk *"), "escape dropped: {joined:?}");
    assert!(joined.contains("underscore _"), "escape dropped: {joined:?}");
    assert!(!joined.contains("\\*"), "the backslash survived: {joined:?}");
}

/// An unterminated `<` is not a tag. The scan ran to end-of-string and deleted
/// everything after it, so one stray `<a href=` removed the rest of the
/// document. Measured before the fix: the sentence stopped at "An unterminated".
#[test]
fn an_unterminated_angle_bracket_does_not_eat_the_document() {
    let doc = "An unterminated <a href= tag must not eat the rest of this text.\n\n\
               A second paragraph that must also survive the stray bracket above.\n";
    let chunks = md::chunk_from_bytes(doc.as_bytes(), "structural", 3, 1, 3, 15).unwrap();
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(
        joined.contains("eat the rest of this text"),
        "the rest of the line was deleted: {joined:?}"
    );
    assert!(
        joined.contains("second paragraph"),
        "the rest of the document was deleted: {joined:?}"
    );
}

/// Real inline HTML must still be stripped — `<em>` is markup, not content.
#[test]
fn real_inline_html_is_still_stripped() {
    let doc = "Some <em>emphasised</em> text inside an ordinary paragraph here.\n";
    let chunks = md::chunk_from_bytes(doc.as_bytes(), "structural", 3, 1, 3, 15).unwrap();
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(joined.contains("emphasised"), "content lost: {joined:?}");
    assert!(!joined.contains("<em>"), "the tag survived: {joined:?}");
}
