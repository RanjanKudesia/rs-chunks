//! Text outside the /CropBox is invisible to a reader and must not be extracted.
//!
//! ISO 32000-1 Table 30: /CropBox is the displayed region, defaulting to
//! /MediaBox. It was never read. Every page of `irs_i1040nr.pdf` declares
//! `/MediaBox [0 0 612 1008]` with `/CropBox [0 0 612 792]` — a 216pt
//! pre-press band above the visible page holding `Userid:`, `Draft`,
//! `Ok to Print`, `Page N of 48` and an internal Windows filesystem path.
//! Because the band tops the MediaBox, the XY-cut sorted it FIRST: the
//! document's markdown opened with printer control marks. Measured before the
//! fix: 107 text placements above the crop line.

use chunks_rs::formats::pdf;

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/pdf")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p.to_str().unwrap().to_string()
}

#[test]
fn pre_press_marks_above_the_crop_are_not_extracted() {
    let md = pdf::to_markdown(&fixture("irs_i1040nr.pdf")).expect("must parse");
    for mark in ["Ok to Print", "Userid:", "Leadpct:", "PAGER/SGML"] {
        assert!(
            !md.contains(mark),
            "out-of-crop pre-press text extracted: {mark:?}"
        );
    }
    // The visible form content must survive.
    assert!(
        md.contains("1040NR"),
        "visible page content lost: {:?}…",
        &md[..md.len().min(160)]
    );
}

/// Control: a document whose CropBox equals its MediaBox (or is absent) must
/// be untouched. tika_testpdf.pdf declares no CropBox.
#[test]
fn a_document_without_a_cropbox_is_unchanged() {
    let md = pdf::to_markdown(&fixture("tika_testpdf.pdf")).expect("must parse");
    assert!(
        md.contains("Tika") && md.contains("Content Analysis"),
        "control document disturbed: {md:?}"
    );
}
