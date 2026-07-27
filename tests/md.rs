//! Fixture-driven Markdown tests over `../test_files/md`, all modes.

use std::path::{Path, PathBuf};

use chunks_rs::formats::md;

fn md_fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("test_files").join("md");
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no .md fixtures in {}", dir.display());
    out
}

const MODES: &[&str] = &["default", "section", "semantic", "sentence", "page_aware", "sliding_window"];

#[test]
fn all_modes_all_fixtures_well_formed() {
    for path in md_fixtures() {
        let p = path.to_str().unwrap();
        for mode in MODES {
            let chunks = md::chunk(p, mode, 3, 1, 3, 15)
                .unwrap_or_else(|e| panic!("md {mode} failed for {p}: {e}"));
            assert!(!chunks.is_empty(), "no chunks: {p} [{mode}]");
            for c in &chunks {
                assert!(!c.content_type.is_empty());
                assert!(c.metadata.is_object(), "metadata must be object: {p} [{mode}]");
            }
        }
    }
}

#[test]
fn to_markdown_returns_source_text() {
    for path in md_fixtures() {
        let p = path.to_str().unwrap();
        let md_text = md::to_markdown(p).unwrap();
        assert!(!md_text.is_empty(), "empty markdown for {p}");
    }
}

#[test]
fn invalid_mode_and_extension_fail_cleanly() {
    let p = md_fixtures()[0].to_str().unwrap().to_string();
    assert!(md::chunk(&p, "nonsense", 3, 1, 3, 15).is_err());
    assert!(md::chunk("foo.pdf", "default", 3, 1, 3, 15).is_err());
}
