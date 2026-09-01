//! A malformed `content.xml` must not come back as a short document.
//!
//! The walker used to `break` on a quick-xml error and return the prefix it had
//! parsed, with `Ok`. That is the worst failure shape a chunking library has:
//! the caller cannot tell "this document is short" from "we lost 95% of it",
//! and nothing in the output hints that anything is missing. L14 established the
//! rule for EPUB — structurally invalid raises, nothing-to-chunk returns `[]` —
//! and this is that rule applied to ODF.

use std::path::{Path, PathBuf};

use chunks_rs::formats::odf;

/// Panics rather than skipping: a fixture-driven test that quietly passes when
/// the corpus is absent pins nothing.
fn fixture(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .join("test_files")
        .join(rel);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

/// Deliberately-broken inputs live outside `test_files/` — see
/// `tests/fixtures_malformed/README.md`. Everything in the main corpus is swept
/// by harnesses that assume a fixture is meant to succeed, so a malformed file
/// there is a permanently red snapshot, not a test.
fn malformed(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures_malformed")
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

/// `derived_truncated_content.odt` is `tika_testFooter.odt` with `content.xml`
/// cut to 40% and left mid-element. Before the fix it returned that 40% as a
/// complete document.
#[test]
fn a_truncated_content_xml_raises_instead_of_returning_a_prefix() {
    let p = malformed("derived_truncated_content.odt");
    for mode in MODES {
        let got = odf::chunk(p.to_str().unwrap(), mode, 3, 1, 3, 15);
        let err = got.expect_err(&format!(
            "{mode}: a malformed content.xml must raise, not return a prefix"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("malformed"),
            "{mode}: the error must say what went wrong, got {msg:?}"
        );
    }
}

/// The intact original must be entirely unaffected — this is what makes the
/// change zero-churn for every well-formed document.
#[test]
fn a_well_formed_document_is_unaffected() {
    let p = fixture("odt/tika_testFooter.odt");
    for mode in MODES {
        let chunks = odf::chunk(p.to_str().unwrap(), mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("{mode}: well-formed odt must still parse, got {e}"));
        assert!(!chunks.is_empty(), "{mode}: expected content");
    }
}

/// The commoner truncation, and the one that used to slip through entirely:
/// the file stops cleanly *between* elements. The XML prefix is well-formed and
/// quick-xml reports EOF, not an error — so before the depth guard this parsed
/// "successfully" and returned 40% of the document with `Ok`.
#[test]
fn a_cut_at_an_element_boundary_is_caught_too() {
    let p = malformed("derived_truncated_at_element.odt");
    for mode in MODES {
        let got = odf::chunk(p.to_str().unwrap(), mode, 3, 1, 3, 15);
        let err = got.expect_err(&format!(
            "{mode}: a document with unclosed elements at EOF is truncated, not short"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("truncated"),
            "{mode}: the error must name the cause, got {msg:?}"
        );
    }
}
