//! Smoke test for chunk_with_images across the heavy formats. Exhaustive parity
//! is in examples/images_dump.rs vs py-chunks; this pins the API into cargo test.

use std::path::{Path, PathBuf};

use chunks_rs::formats;

fn first_with_ext(ext: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, ext: &str) -> Option<PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in &entries {
            if p.is_dir() {
                if let Some(f) = walk(p, ext) {
                    return Some(f);
                }
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case(ext))
                == Some(true)
                && std::fs::metadata(p)
                    .map(|m| m.len() < 10 * 1024 * 1024)
                    .unwrap_or(false)
            {
                return Some(p.clone());
            }
        }
        None
    }
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("test_files"),
        ext,
    )
}

fn assert_well_formed(chunks: &[chunks_rs::Chunk], images: &[(String, Vec<u8>)]) {
    for c in chunks {
        assert!(!c.content_type.is_empty());
        assert!(c.metadata.is_object());
    }
    for (name, bytes) in images {
        assert!(!name.is_empty());
        assert!(!bytes.is_empty(), "image {name} has no bytes");
    }
}

#[test]
fn chunk_with_images_heavy_formats() {
    if let Some(p) = first_with_ext("docx") {
        let (c, i) =
            formats::docx::chunk_with_images(p.to_str().unwrap(), "default", 3, 1, 3, 15).unwrap();
        assert_well_formed(&c, &i);
    }
    if let Some(p) = first_with_ext("pptx") {
        let (c, i) =
            formats::pptx::chunk_with_images(p.to_str().unwrap(), "default", 3, 1, 3, 15).unwrap();
        assert_well_formed(&c, &i);
    }
    if let Some(p) = first_with_ext("xlsx") {
        let (c, i) = formats::xlsx::chunk_with_images(
            p.to_str().unwrap(),
            "row",
            1,
            3,
            1,
            true,
            Vec::new(),
            true,
            2000,
        )
        .unwrap();
        assert_well_formed(&c, &i);
    }
    if let Some(p) = first_with_ext("doc") {
        let (c, i) =
            formats::doc::chunk_with_images(p.to_str().unwrap(), "default", 3, 1, 3, 15).unwrap();
        assert_well_formed(&c, &i);
    }
    if let Some(p) = first_with_ext("ppt") {
        let (c, i) =
            formats::ppt::chunk_with_images(p.to_str().unwrap(), "default", 3, 1, 3, 15).unwrap();
        assert_well_formed(&c, &i);
    }
}

#[test]
fn to_markdown_with_images_heavy_formats() {
    type MarkdownRenderer = fn(&str) -> chunks_rs::Result<chunks_rs::MarkdownWithImages>;
    let cases: &[(&str, MarkdownRenderer)] = &[
        ("docx", |p| formats::docx::to_markdown_with_images(p)),
        ("pptx", |p| formats::pptx::to_markdown_with_images(p)),
        ("xlsx", |p| formats::xlsx::to_markdown_with_images(p)),
        ("doc", |p| formats::doc::to_markdown_with_images(p)),
        ("ppt", |p| formats::ppt::to_markdown_with_images(p)),
    ];
    for (ext, f) in cases {
        if let Some(p) = first_with_ext(ext) {
            let (_md, images) = f(p.to_str().unwrap()).unwrap();
            // image names must be unique (matches py-chunks dict semantics)
            let mut names: Vec<&String> = images.iter().map(|(n, _)| n).collect();
            let n = names.len();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), n, "duplicate image names for {ext}");
        }
    }
}
