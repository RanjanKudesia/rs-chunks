//! A `text:a` left open across a paragraph boundary must not slice unrelated text.
//!
//! `Writer::link` holds `(href, byte index into `text`)`. `flush_paragraph`
//! clears `text`, which invalidates that index — but the index survived, so a
//! link still open when a paragraph ended resolved against the *next*
//! paragraph's bytes. Two consequences, both real:
//!
//!   1. The label was whatever the following paragraph happened to contain.
//!   2. If the stale index landed mid-codepoint, `w.text[start..]` panicked.
//!      That surfaced through PyO3 as `pyo3_runtime.PanicException`, which
//!      derives from `BaseException` — so a caller's `except Exception` did not
//!      catch it — and on `wasm32-unknown-unknown` (`panic-strategy: abort`) it
//!      was an abort, because `catch_unwind` is dead code there.
//!
//! The input below is **well-formed ODF**, not a fuzz artifact:
//! `office:annotation` legally contains its own `text:p`, and a comment inside
//! a hyperlink is an ordinary thing for an author to write. The accented
//! characters that follow put the stale index inside a two-byte sequence.

use std::io::{Cursor, Write};

use chunks_rs::formats::odf;
use zip::write::SimpleFileOptions;

/// Build a minimal, valid `.odt` in memory around the supplied body XML.
fn odt_with_body(body: &str) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
 xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:xlink="http://www.w3.org/1999/xlink">
<office:body><office:text>{body}</office:text></office:body>
</office:document-content>"#
    );

    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zw.start_file("mimetype", stored).expect("mimetype entry");
    zw.write_all(b"application/vnd.oasis.opendocument.text")
        .expect("mimetype bytes");
    let deflated = SimpleFileOptions::default();
    zw.start_file("META-INF/manifest.xml", deflated)
        .expect("manifest entry");
    zw.write_all(
        br#"<?xml version="1.0"?><manifest:manifest
 xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
<manifest:file-entry manifest:full-path="/"
 manifest:media-type="application/vnd.oasis.opendocument.text"/>
</manifest:manifest>"#,
    )
    .expect("manifest bytes");
    zw.start_file("content.xml", deflated)
        .expect("content entry");
    zw.write_all(content.as_bytes()).expect("content bytes");
    zw.finish().expect("finish zip").into_inner()
}

/// The panic case. Before the fix this aborted the test binary's thread with
/// "byte index 1 is not a char boundary"; the assertion below never ran.
#[test]
fn a_link_open_across_a_paragraph_does_not_panic() {
    let body = concat!(
        "<text:p>A<text:a xlink:href=\"http://example.invalid\">B",
        "<office:annotation><text:p>N</text:p></office:annotation>",
        "éé</text:a></text:p>"
    );
    let md = odf::to_markdown_from_bytes(&odt_with_body(body), "probe.odt")
        .expect("a well-formed .odt must parse");

    // The body text survives. The exact link rendering is deliberately not
    // asserted — a link whose start index was invalidated has no correct label,
    // and dropping it is the honest outcome.
    assert!(md.contains('é'), "lost the paragraph text: {md:?}");
}

/// The label must come from the link's own paragraph, never text that arrived
/// after it. This is the half a char-boundary check alone would NOT have fixed:
/// every index here is a valid boundary, so the slice succeeds and silently
/// produces the wrong string.
///
/// The text after the nested paragraph is what makes this bite. `start` is 2
/// ("AA"); the annotation's `</text:p>` clears `text`; then "CCCC" accumulates,
/// so at `</text:a>` the guard `start <= text.len()` passes (2 <= 4) and the
/// label becomes bytes 2.. of "CCCC" — text the link never covered.
#[test]
fn a_stale_link_does_not_steal_text_that_arrived_after_it() {
    let body = concat!(
        "<text:p>AA<text:a xlink:href=\"http://example.invalid\">BB",
        "<office:annotation><text:p>NN</text:p></office:annotation>",
        "CCCC</text:a></text:p>"
    );
    let md = odf::to_markdown_from_bytes(&odt_with_body(body), "probe.odt")
        .expect("a well-formed .odt must parse");

    assert!(
        !md.contains("](http://example.invalid)"),
        "a link was rendered from an invalidated start index: {md:?}"
    );
    assert!(
        md.contains("CCCC"),
        "the post-annotation text was mangled by the stale slice: {md:?}"
    );
}

/// A link that opens and closes inside one paragraph must still render. The
/// fix clears `link` on paragraph flush, so this pins that the ordinary case
/// was not broken in the process.
#[test]
fn an_ordinary_link_still_renders() {
    let body = "<text:p>see <text:a xlink:href=\"http://example.invalid\">the docs</text:a> now</text:p>";
    let md = odf::to_markdown_from_bytes(&odt_with_body(body), "probe.odt")
        .expect("a well-formed .odt must parse");

    assert!(
        md.contains("[the docs](http://example.invalid)"),
        "an in-paragraph link stopped rendering: {md:?}"
    );
}
