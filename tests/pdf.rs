//! PDF parsing, against the real corpus.
//!
//! These pin the behaviour the owned parser was built for ([#57], [#74]): images
//! nested in Form XObjects, text recovered from fonts that carry their encoding
//! in the font program rather than the dictionary, and multi-column reading
//! order. Every assertion is measured against a fixture, not a count.

use std::path::PathBuf;

use chunks_rs::formats::pdf;

fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "test_files", "pdf", name].iter().collect();
    path.to_string_lossy().to_string()
}

fn markdown(name: &str) -> String {
    pdf::to_markdown(&fixture(name)).expect("markdown")
}

fn images(name: &str) -> Vec<(String, Vec<u8>)> {
    pdf::to_markdown_with_images(&fixture(name)).expect("images").1
}

/// [#57]: the page's `/XObject` offers one *Form*, and the five images live in
/// the form's own resources. Walking the content stream is what finds them.
#[test]
fn images_nested_in_a_form_xobject_are_extracted() {
    let images = images("pdfjs_images.pdf");
    assert_eq!(images.len(), 5, "{:?}", images.iter().map(|(n, _)| n).collect::<Vec<_>>());
    for (name, bytes) in &images {
        assert!(name.starts_with("image_p1_"), "{name}");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{name} is not a PNG");
    }
}

/// Every `![](…)` the markdown emits must name a file `list_images` returns —
/// the two entry points may not disagree about what a page holds.
#[test]
fn every_image_reference_resolves_to_an_extracted_image() {
    for name in ["pdfjs_images.pdf", "pdfjs_S2.pdf", "arxiv_1706.03762_attention.pdf", "sample-pdf.pdf"] {
        let (markdown, images) = pdf::to_markdown_with_images(&fixture(name)).expect("parse");
        let keys: Vec<&str> = images.iter().map(|(n, _)| n.as_str()).collect();
        for reference in markdown.match_indices("![](").map(|(i, _)| {
            let rest = &markdown[i + 4..];
            &rest[..rest.find(')').unwrap_or(0)]
        }) {
            assert!(keys.contains(&reference), "{name}: markdown references {reference}, which is not extracted");
        }
    }
}

/// A JPEG is passed through as the file it already is, rather than re-encoded.
#[test]
fn a_jpeg_image_is_returned_untouched() {
    let images = images("sample-pdf.pdf");
    let (name, bytes) = images.iter().find(|(n, _)| n.ends_with(".jpg")).expect("a jpeg");
    assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "{name} has no JPEG SOI");
    assert!(bytes.ends_with(&[0xFF, 0xD9]), "{name} has no JPEG EOI");
}

/// TeX's maths fonts are symbolic, name no `/Encoding`, and carry no
/// `/ToUnicode`; their encoding is in the clear inside the font program. Reading
/// it recovers 28,000 characters on this fixture alone.
#[test]
fn text_is_recovered_from_a_font_programs_own_encoding() {
    let text = markdown("arxiv_2005.14165_gpt3.pdf");
    assert!(text.contains("Language Models are Few-Shot Learners"), "title missing");
    assert!(text.len() > 150_000, "only {} characters", text.len());
}

/// A two-column page must read down each column. Sorting by baseline instead
/// welds each left-hand line to the right-hand line beside it.
#[test]
fn a_two_column_page_reads_down_its_columns() {
    let text = markdown("arxiv_1810.04805_bert.pdf");
    assert!(
        text.contains("we also use a “next sentence prediction” task that jointly pretrains text-pair representations"),
        "column text is interleaved"
    );
}

/// A ligature glyph must not leave the word unsearchable.
#[test]
fn ligatures_are_spelled_out() {
    let text = markdown("arxiv_1409.1556_vgg.pdf");
    assert!(text.contains("classification"), "ligature not expanded");
    assert!(!text.contains('\u{FB01}'), "a ﬁ ligature survived");
}

/// A hyperlink's target is in `/Annots` and nowhere in the content stream, so
/// without annotation support the URL is simply gone.
#[test]
fn link_annotations_become_markdown_links() {
    let text = markdown("arxiv_1810.04805_bert.pdf");
    assert!(text.contains("](https://github.com/google-research/bert)"), "link target missing");
}

/// [#56]: a scanned PDF has no text to chunk, and says so.
#[test]
fn a_text_less_pdf_reports_that_it_has_no_text() {
    let error = pdf::to_markdown(&fixture("large-doc.pdf")).expect_err("should refuse");
    assert!(error.to_string().contains("no extractable text"), "{error}");
}

/// …but asking for images returns the page scans it does have.
#[test]
fn a_scanned_pdf_still_yields_its_page_images() {
    let images = images("large-doc.pdf");
    assert_eq!(images.len(), 100);
    assert!(images.iter().all(|(_, b)| b.len() > 1_000));
}

/// The path and bytes entry points are the same code, and must stay so.
#[test]
fn reading_from_bytes_matches_reading_from_a_path() {
    let name = "arxiv_1706.03762_attention.pdf";
    let bytes = std::fs::read(fixture(name)).expect("read");
    assert_eq!(pdf::to_markdown_from_bytes(&bytes).unwrap(), markdown(name));
    assert_eq!(
        pdf::chunk_from_bytes(&bytes, "default", 3, 1, 3, 15).unwrap().len(),
        pdf::chunk(&fixture(name), "default", 3, 1, 3, 15).unwrap().len()
    );
}

/// Not a defect, but a limit worth pinning: a CFF subset whose glyph names are
/// bare indices (`/g18`) and which carries no `/ToUnicode` cannot be decoded
/// without the original font. The parser drops those glyphs rather than
/// emitting mojibake, so what it *does* return is still correct prose.
#[test]
fn an_undecodable_subset_font_degrades_to_silence_not_to_noise() {
    let text = markdown("pdfjs_TAMReview.pdf");
    assert!(text.contains("Overview of the Technology Acceptance Model"), "decodable text missing");
    let replacement = text.chars().filter(|c| *c == '\u{FFFD}').count();
    assert_eq!(replacement, 0, "replacement characters leaked into the output");
}

/// Every fixture must parse or fail cleanly — never panic.
#[test]
fn the_whole_corpus_parses_without_panicking() {
    let dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "test_files", "pdf"].iter().collect();
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).expect("fixtures").filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        seen += 1;
        let _ = pdf::chunk(&path.to_string_lossy(), "default", 3, 1, 3, 15);
    }
    assert_eq!(seen, 24, "the PDF corpus changed size");
}
