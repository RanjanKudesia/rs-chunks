//! Fixture-driven RTF tests over the real corpus in `../test_files/rtf`.
//!
//! Each assertion below pins a defect that was real: emphasis discarded (#59),
//! body headings never detected (#60), symbol-font bullets leaking U+FFFD/PUA
//! (#61), and `document_metadata.author` always null (#62).

use std::path::{Path, PathBuf};

use chunks_rs::formats::rtf;

const MODES: [&str; 7] = [
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join("rtf")
}

fn fixture(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn fixtures() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("rtf fixture dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("rtf"))
        })
        .collect();
    out.sort();
    assert!(out.len() >= 10, "expected a real fixture corpus, got {}", out.len());
    out
}

fn markdown(name: &str) -> String {
    rtf::to_markdown(fixture(name).to_str().unwrap()).expect("markdown")
}

fn doc(name: &str) -> rtf::extract::RtfDoc {
    rtf::extract::extract(&std::fs::read(fixture(name)).expect("read fixture"))
}

// ── #59 — bold/italic survive as markdown emphasis ──────────────────────────

#[test]
fn bold_and_italic_become_markdown_emphasis() {
    let md = markdown("tika_testRTFBoldItalic.rtf");
    let lines: Vec<&str> = md.lines().collect();
    assert_eq!(lines[0], "**bold**");
    assert_eq!(lines[1], "**bold** ***italic***");
    assert_eq!(lines[3], "*italic*");
    assert_eq!(lines[4], "**bold then** ***italic then*** *not bold*");
}

#[test]
fn emphasis_tracks_the_run_it_belongs_to() {
    // Bytes buffer across group boundaries; flushing late once put the markers
    // around the wrong span, bolding a whole paragraph from one `\b` run.
    let md = markdown("tika_testRTFHyperlink.rtf");
    assert!(md.contains("**Type a question for help**"), "{md}");
    assert!(md.contains("**Contact Us**"), "{md}");
    assert!(
        md.contains("*to help you access, prioritize, and act on communications and information*"),
        "{md}"
    );
}

/// Emphasis runs on a line, as (byte offset, run length), in order.
fn emphasis_runs(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut runs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'*' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'*' {
                i += 1;
            }
            runs.push((start, i - start));
        } else {
            i += 1;
        }
    }
    runs
}

#[test]
fn emphasis_markers_never_wrap_whitespace_or_stand_empty() {
    // A marker that opens on a space, closes after one, or wraps nothing is not
    // emphasis to a markdown parser — it survives into the text as literal `*`.
    for path in fixtures() {
        let md = rtf::to_markdown(path.to_str().unwrap()).expect("markdown");
        let name = path.file_name().unwrap().to_string_lossy();
        for line in md.lines() {
            let runs = emphasis_runs(line);
            assert_eq!(
                runs.len() % 2,
                0,
                "unbalanced emphasis in {name}: {line}"
            );
            for pair in runs.chunks(2) {
                let (open, open_len) = pair[0];
                let (close, close_len) = pair[1];
                assert_eq!(open_len, close_len, "mismatched run in {name}: {line}");
                let inner = &line[open + open_len..close];
                assert!(!inner.is_empty(), "empty emphasis span in {name}: {line}");
                assert!(
                    !inner.starts_with(char::is_whitespace),
                    "emphasis opens on whitespace in {name}: {line}"
                );
                assert!(
                    !inner.ends_with(char::is_whitespace),
                    "emphasis closes after whitespace in {name}: {line}"
                );
            }
        }
    }
}

// ── #60 — body headings from paragraph styles ───────────────────────────────

#[test]
fn heading_styles_become_headings() {
    // LibreOffice keeps `heading 1/2/3` styles when converting the POI original.
    let md = markdown("conv_libreoffice_heading123.rtf");
    let lines: Vec<&str> = md.lines().collect();
    assert_eq!(lines[0], "# First paragraph");
    assert!(lines.contains(&"## Second paragraph"), "{md}");
    assert!(lines.contains(&"### Third paragraph"), "{md}");
}

#[test]
fn heading_marker_does_not_bleed_onto_the_next_paragraph() {
    let md = markdown("conv_libreoffice_heading123.rtf");
    for line in md.lines().filter(|l| l.starts_with('#')) {
        assert!(
            line.len() < 60,
            "a body paragraph was marked as a heading: {line}"
        );
    }
    assert_eq!(md.lines().filter(|l| l.starts_with('#')).count(), 3, "{md}");
}

#[test]
fn a_document_without_styles_gets_no_headings() {
    // Apple's `textutil` drops the style sheet entirely, so there is nothing to
    // detect — inventing a heading here would be worse than finding none.
    let md = markdown("conv_apple_heading123.rtf");
    assert!(md.starts_with("First paragraph"), "{md}");
    assert!(!md.contains('#'), "{md}");
}

// ── #61 — symbol-font bullets ───────────────────────────────────────────────

#[test]
fn list_markers_are_markdown_not_symbol_glyphs() {
    for name in [
        "tika_testRTFListLibreOffice.rtf",
        "tika_testRTFListMicrosoftWord.rtf",
    ] {
        let md = markdown(name);
        assert!(md.contains("- first"), "{name}: {md}");
        assert!(md.contains("1. one"), "{name}: {md}");
    }
    // Wingdings writes its bullet as the Private Use codepoint U+F0FC.
    let md = markdown("tika_testRTFJapanese.rtf");
    assert!(md.contains("- 太平洋戦争を前に"), "{md}");
}

#[test]
fn no_replacement_or_private_use_characters_reach_the_text() {
    for path in fixtures() {
        let md = rtf::to_markdown(path.to_str().unwrap()).expect("markdown");
        let name = path.file_name().unwrap().to_string_lossy();
        for ch in md.chars() {
            assert_ne!(ch, '\u{FFFD}', "replacement char in {name}");
            assert!(
                !('\u{E000}'..='\u{F8FF}').contains(&ch),
                "private use char {:?} in {name}",
                ch
            );
        }
    }
}

// ── #62 — document_metadata.author ──────────────────────────────────────────

#[test]
fn author_is_recovered_from_the_info_group() {
    assert_eq!(
        doc("tika_testRTFBoldItalic.rtf").author.as_deref(),
        Some("Michael McCandless")
    );
    // `\'f6` must decode in the document's own code page, not be dropped.
    assert_eq!(
        doc("tika_testRTFListLibreOffice.rtf").author.as_deref(),
        Some("Axel Dörfler")
    );
    // A `\upr` pair: the flat ANSI copy is the one this scan may safely read.
    assert_eq!(
        doc("conv_libreoffice_heading123.rtf").author.as_deref(),
        Some("Paolo Mottadelli")
    );
}

#[test]
fn a_document_without_an_info_group_has_no_author() {
    assert_eq!(doc("tika_testRTFHyperlink.rtf").author, None);
}

#[test]
fn author_reaches_document_metadata() {
    let (chunks, _) = rtf::chunk(
        fixture("tika_testRTFBoldItalic.rtf").to_str().unwrap(),
        "semantic",
        512,
        50,
        3,
        3,
    )
    .map(|c| (c, ()))
    .expect("chunk");
    let meta = &chunks[0].metadata["document_metadata"];
    assert_eq!(meta["author"], "Michael McCandless");
    assert_eq!(meta["source_type"], "rtf");
}

// ── the title pre-scan must not regress ─────────────────────────────────────

#[test]
fn a_nested_title_is_refused_rather_than_guessed() {
    // The Japanese fixture stores its title as a `\upr` pair whose ANSI copy is
    // `ゾ?ル?ゲ?`; returning nothing beats returning that.
    assert_eq!(doc("tika_testRTFJapanese.rtf").title, None);
    assert_eq!(doc("tika_testRTF-ms932.rtf").title.as_deref(), Some("タイトル"));
}

// ── every fixture still chunks in every mode ────────────────────────────────

#[test]
fn all_modes_all_fixtures_well_formed() {
    for path in fixtures() {
        let p = path.to_str().unwrap();
        for mode in MODES {
            let chunks = rtf::chunk(p, mode, 512, 50, 3, 3)
                .unwrap_or_else(|e| panic!("{p} [{mode}] failed: {e}"));
            for c in &chunks {
                assert!(!c.content_type.is_empty(), "{p} [{mode}] empty content_type");
                assert!(c.metadata.is_object(), "{p} [{mode}] metadata not an object");
            }
        }
    }
}
