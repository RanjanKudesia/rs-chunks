//! Styling and scripting parts are resources, not body text.
//!
//! `tika_testRFC822_oddfrom.eml` is a `multipart/related` HTML message whose
//! three `text/css` parts are resources the root part references (RFC 2387).
//! They were inlined as prose: 44 of 54 chunks were pure CSS — 94.5% of the
//! rendered output, each one a confident, meaningless embedding. The
//! printability gate is structurally unable to catch this (CSS is printable);
//! the subtype is the discriminator.

use chunks_rs::formats::eml;

fn fixture() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/eml/tika_testRFC822_oddfrom.eml");
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p.to_str().unwrap().to_string()
}

#[test]
fn css_resource_parts_are_not_inlined() {
    let md = eml::to_markdown(&fixture()).expect("must parse");
    assert!(
        !md.contains("FLOAT: right") && !md.contains("font-family"),
        "stylesheet text reached the body: {:?}…",
        &md[..md.len().min(200)]
    );
    // The message's real content must survive the gate.
    assert!(
        md.contains("Air Permit"),
        "the actual subject/body was lost: {:?}…",
        &md[..md.len().min(200)]
    );
}
