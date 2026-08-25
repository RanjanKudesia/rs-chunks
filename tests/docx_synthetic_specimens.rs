//! Spec-legal WordprocessingML that the engine mishandled.
//!
//! These specimens exercise features no real corpus file does. Two of them were
//! not "unsupported" but actively wrong: `oddmain.docx` is a conformant DOCX the
//! engine could not open at all, and `ruby.docx` came out with its text
//! corrupted rather than missing.

use std::path::{Path, PathBuf};

use chunks_rs::formats::docx;

/// Panics rather than skipping. A fixture-driven test that silently passes when
/// the corpus is absent pins nothing, and would let these gaps reopen unseen.
fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .join("test_files/docx_synthetic")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

fn text_of(name: &str, mode: &str) -> Vec<String> {
    let p = fixture(name);
    docx::chunk(p.to_str().unwrap(), mode, 3, 1, 3, 15)
        .unwrap_or_else(|e| panic!("{name} [{mode}]: {e}"))
        .into_iter()
        .map(|c| c.content)
        .collect()
}

/// The main part is whatever `_rels/.rels` points at — `word/document.xml` is
/// only Word's convention. This package calls it `word/mydoc.xml`, which is
/// spec-legal, and the engine used to fail it outright with
/// "word/document.xml not found in DOCX".
#[test]
fn a_main_part_that_is_not_named_document_xml_still_opens() {
    for mode in ["default", "structural", "section", "semantic"] {
        let chunks = text_of("oddmain.docx", mode);
        assert!(
            chunks.iter().any(|c| c.contains("text")),
            "oddmain [{mode}]: expected the body text, got {chunks:?}"
        );
    }
}

/// Every other Word fixture resolves to `word/document.xml`, so the resolver
/// must be a no-op for them — that is what makes this change zero-churn.
#[test]
fn the_conventional_main_part_is_unaffected() {
    for name in ["fldsimple.docx", "omath.docx", "strict.docx", "zip64.docx"] {
        let chunks = text_of(name, "structural");
        assert!(!chunks.is_empty(), "{name}: expected content, got none");
    }
}

/// `<w:rt>` is the *phonetic reading*, `<w:rubyBase>` is the word. Both are
/// `<w:r><w:t>`, so the walker emitted the reading as body text in front of the
/// word: "furigana base". For real Japanese that is ふりがな 漢字 — the reading
/// duplicated ahead of the kanji, in every mode. Only the base is the document's
/// text.
#[test]
fn a_ruby_annotation_keeps_the_base_and_drops_the_reading() {
    for mode in ["default", "structural", "section", "semantic"] {
        let joined = text_of("ruby.docx", mode).join(" ");
        assert!(
            joined.contains("base"),
            "ruby [{mode}]: the base text must survive, got {joined:?}"
        );
        assert!(
            !joined.contains("furigana"),
            "ruby [{mode}]: the phonetic reading must not be emitted as body \
             text, got {joined:?}"
        );
    }
}
