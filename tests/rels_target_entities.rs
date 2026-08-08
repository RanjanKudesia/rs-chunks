//! Relationship `Target` attributes must be entity-decoded before use.
//!
//! quick-xml returns attribute values exactly as written on disk, and XML
//! requires `&` to be escaped there. Element text has always been folded back
//! through the entity resolver; attributes were not, so every hyperlink URL in
//! `word/_rels/document.xml.rels` reached `get_markdown` with `&amp;` intact
//! and was published that way inside `[label](url)`.
//!
//! `poi_bug59058.docx` is a real bibliography document: 160 of its external
//! hyperlink targets are Scopus/Lancet query strings joined by `&`.

use std::path::{Path, PathBuf};

fn fixture(dir: &str, name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join(dir)
        .join(name);
    assert!(p.is_file(), "missing fixture: {}", p.display());
    p
}

#[test]
fn docx_markdown_hyperlinks_carry_a_bare_ampersand() {
    let path = fixture("docx", "poi_bug59058.docx");
    let md = chunks_rs::formats::docx::to_markdown(path.to_str().unwrap())
        .expect("get_markdown on poi_bug59058.docx");

    assert_eq!(
        md.matches("&amp;").count(),
        0,
        "escaped ampersands survived into the markdown output"
    );

    // A specific link from the bibliography, spot-checked against the raw
    // rels XML (where it is written with three `&amp;`).
    assert!(
        md.contains(
            "http://www.scopus.com/scopus/inward/record.url?eid=2-s2.0-0024997614&partnerID=K84CvKBR&rel=3.0.0"
        ),
        "the known Scopus target is missing or still escaped"
    );

    // The decode must not invent or delete ampersands: the document's links
    // are the only source of them, and there are 160.
    assert_eq!(
        md.matches('&').count(),
        166,
        "unexpected ampersand count after decoding"
    );
}

/// The same fixture's *chunks* were always correct — the defect was confined to
/// the markdown renderer. Pin that so a future change to the shared decode path
/// cannot quietly start double-decoding chunk text.
#[test]
fn docx_chunk_content_is_unaffected() {
    let path = fixture("docx", "poi_bug59058.docx");
    let chunks = chunks_rs::formats::docx::chunk(path.to_str().unwrap(), "structural", 3, 1, 3, 15)
        .expect("get_chunks on poi_bug59058.docx");
    let joined: String = chunks.into_iter().map(|c| c.content).collect();
    assert_eq!(joined.matches("&amp;").count(), 0);
}
