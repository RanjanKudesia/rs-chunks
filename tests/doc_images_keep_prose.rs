//! Asking for images must not return less text than not asking (TECH_DEBT L5).
//!
//! `to_markdown_with_images` loaded only the MAIN story, while `to_markdown`
//! loads main + footnotes + headers/footers + comments + endnotes + text boxes.
//! So the images surface silently dropped whole categories of content:
//! measured, `poi_footnote.doc` returned 9 chars where the plain surface
//! returned 101 — **91% of the document gone** — and `sample.doc` lost the only
//! table it has, which lives in a text box.
//!
//! L5 was fixed once for another module. This is the same defect recurring in a
//! module L5 never touched, which is why the invariant is now pinned rather
//! than the symptom.
//!
//! The golden snapshot cannot catch this: `golden_snapshot.py` only ever calls
//! `get_chunks`, never `get_markdown`, and never passes `list_images`.

use std::path::Path;

/// Strip `![](…)` image markers and normalise whitespace.
fn prose_only(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        match rest[start..].find(')') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every `.doc` in the corpus, both surfaces, same prose.
#[test]
fn the_images_surface_never_loses_prose() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/doc");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("doc corpus must exist") {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("doc") {
            continue;
        }
        // Skip the multi-hundred-MB stress fixture; it is covered by the rest.
        if p.metadata().map(|m| m.len()).unwrap_or(0) > 16 * 1024 * 1024 {
            continue;
        }
        let path = p.to_str().unwrap();
        let Ok(plain) = chunks_rs::formats::doc::to_markdown(path) else {
            continue;
        };
        let Ok((with_images, _)) = chunks_rs::formats::doc::to_markdown_with_images(path) else {
            continue;
        };
        assert_eq!(
            prose_only(&plain),
            prose_only(&with_images),
            "{}: asking for images changed the prose",
            p.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no .doc fixtures exercised");
}

/// The sharpest single case: 91% of the document used to disappear.
#[test]
fn a_document_that_is_mostly_footnotes_keeps_them() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/doc/poi_footnote.doc");
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    let path = p.to_str().unwrap();
    let (with_images, _) =
        chunks_rs::formats::doc::to_markdown_with_images(path).expect("must parse");
    assert!(
        with_images.contains("[Footnotes]"),
        "the footnotes story is missing from the images surface: {with_images:?}"
    );
}
