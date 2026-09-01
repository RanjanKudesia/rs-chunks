//! A slide-chrome field's cached value is not slide content.
//!
//! `<a:fld type="slidenum">` (and `datetime*`, `ftr`) carries whatever the
//! value happened to be at save time. It was extracted as ordinary run text,
//! so slide 10 of `poi_2411-Performance_Up.pptx` — whose only non-image text
//! is its number placeholder — rendered a body of `- 10` and notes of `10`,
//! and the `.potx` corpus put 49 stale slide numbers into notes lines.

use chunks_rs::formats::pptx;

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/pptx")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p.to_str().unwrap().to_string()
}

#[test]
fn a_slidenum_field_is_not_body_text() {
    let md = pptx::to_markdown(&fixture("poi_2411-Performance_Up.pptx")).expect("must parse");
    // Slide 10's section must not contain the bare cached number as content.
    let sec = md
        .split("Slide 10")
        .nth(1)
        .and_then(|s| s.split("Slide 11").next())
        .expect("slide 10 section");
    assert!(
        !sec.lines().any(|l| l.trim() == "- 10" || l.trim() == "10"),
        "slidenum cache emitted as content: {sec:?}"
    );
    // Real slide text elsewhere must be intact.
    assert!(
        md.contains("Monitoring Your Server"),
        "slide titles lost: {:?}…",
        &md[..md.len().min(160)]
    );
}
