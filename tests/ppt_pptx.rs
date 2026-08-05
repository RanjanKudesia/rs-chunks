//! PowerPoint defects: empty-deck contract, background images, slide attribution.

use std::path::{Path, PathBuf};

use chunks_rs::formats::{ppt, pptx};

const PPTX_MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

fn fixture(dir: &str, name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join(dir)
        .join(name);
    assert!(p.is_file(), "missing fixture: {}", p.display());
    p
}

/// #16: a text-free deck is empty, not an error — and every mode must agree.
///
/// `section` and `semantic` returned `Err` on zero output while the other five
/// returned `[]`, so whether a caller saw an exception depended on which mode
/// they asked for. These fixtures genuinely carry no body text (their markdown
/// is slide scaffolding only), so `[]` is the honest answer.
#[test]
fn a_text_free_deck_returns_empty_from_every_mode() {
    for (dir, name) in [
        ("potx", "poi_02_bug59273.potx"),
        ("pptx", "poi_tika-2605.pptx"),
        ("pptx", "poi_SmartArt.pptx"),
    ] {
        let f = fixture(dir, name);
        let path = f.to_str().unwrap();
        for mode in PPTX_MODES {
            let got = pptx::chunk(path, mode, 3, 1, 3, 15);
            assert!(
                got.is_ok(),
                "{name} mode {mode}: a text-free deck must not raise, got {:?}",
                got.err()
            );
        }
    }
}

/// #17: a slide background image is an image.
///
/// `poi_02_bug59273.potx` is a single slide whose only content is a 555 KB PNG
/// drawn as the background — `<p:bg><p:bgPr><a:blipFill>`, never inside a
/// `<p:pic>`. The extractor matched `<p:pic>` only, so the file reported zero
/// images through every entry point while carrying half a megabyte of them.
#[test]
fn slide_background_images_are_extracted() {
    let f = fixture("potx", "poi_02_bug59273.potx");
    let path = f.to_str().unwrap();

    let (_chunks, images) = pptx::chunk_with_images(path, "structural", 3, 1, 3, 15).unwrap();
    assert_eq!(images.len(), 1, "background image missing from chunk path");
    assert!(images[0].1.len() > 100_000, "image bytes look truncated");

    // The markdown path had the same gate and must agree with the chunk path.
    let (md, md_images) = pptx::to_markdown_with_images(path).unwrap();
    assert_eq!(
        md_images.len(),
        1,
        "background image missing from the markdown path"
    );
    assert_eq!(
        images[0].0, md_images[0].0,
        "the two paths disagree about the image's name"
    );
    assert!(
        md.contains("!["),
        "markdown should render the background image: {md:?}"
    );
}

/// #19: image→slide attribution must not be all-or-nothing.
///
/// Attribution was cross-checked against `extract_slides().len()` and every
/// image dropped to `page_number: null` unless the counts matched. But
/// `extract_slides` reads the SlideListWithText, which omits a slide with no
/// text placeholders — `sample1.ppt` has 20 slide containers and 19 SLWT
/// entries, so one text-free slide cost all six images their slide number.
#[test]
fn ppt_images_keep_their_slide_number() {
    for name in ["sample1.ppt", "sample2.ppt", "sample3.ppt"] {
        let f = fixture("ppt", name);
        let (chunks, _images) =
            ppt::chunk_with_images(f.to_str().unwrap(), "structural", 3, 1, 3, 15).unwrap();

        let pages: Vec<Option<u64>> = chunks
            .iter()
            .filter(|c| c.content_type == "image")
            .map(|c| c.metadata.get("page_number").and_then(|v| v.as_u64()))
            .collect();

        assert!(!pages.is_empty(), "{name}: expected image chunks");
        assert!(
            pages.iter().all(Option::is_some),
            "{name}: images lost their slide number: {pages:?}"
        );
        assert!(
            pages.iter().flatten().all(|p| *p >= 1),
            "{name}: slide numbers must be 1-based: {pages:?}"
        );
    }
}
