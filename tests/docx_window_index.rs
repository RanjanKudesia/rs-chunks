//! `window_index` counts WINDOWS, 0-based — the published contract.
//!
//! `chunking-modes/sliding-window.mdx` documents it as "which window this is,
//! 0-based", and every sibling module (txt, md, html, pptx, xlsx, csv) counts
//! windows. An uncommitted draft changed docx to count emitted chunks instead,
//! citing an SDK contract "asserted by test_window_index_increments_from_zero"
//! — a test that existed nowhere in the workspace. This file is that test,
//! made real, and it pins the per-window semantics: when the size cap splits
//! one window into several chunks, the parts SHARE the window's index.
//!
//! The golden snapshot can never see this — `digest()` hashes metadata keys,
//! not values — so this test is the only oracle for the numbering.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;

fn docx_with_paragraphs(paras: &[String]) -> Vec<u8> {
    let mut body = String::new();
    for p in paras {
        body.push_str(&format!("<w:p><w:r><w:t>{p}</w:t></w:r></w:p>"));
    }
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{body}</w:body></w:document>"#
    );
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let d = SimpleFileOptions::default();
    zw.start_file("_rels/.rels", d).unwrap();
    zw.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zw.start_file("word/document.xml", d).unwrap();
    zw.write_all(doc.as_bytes()).unwrap();
    zw.finish().unwrap().into_inner()
}

#[test]
fn window_index_counts_windows_and_split_parts_share_one() {
    // Five paragraphs; the third is far past MAX_WINDOW_CONTENT_CHARS (6,000),
    // so with window_size=2, overlap=0 the middle window must split into
    // several chunks that all carry window_index 1.
    let huge = "sentence with sufficient words to split on repeatedly. ".repeat(300);
    let paras: Vec<String> = vec![
        "first ordinary paragraph of text".into(),
        "second ordinary paragraph of text".into(),
        huge,
        "fourth ordinary paragraph of text".into(),
        "fifth ordinary paragraph of text".into(),
    ];
    let bytes = docx_with_paragraphs(&paras);
    let chunks = chunks_rs::formats::docx::chunk_from_bytes(&bytes, "sliding_window", 2, 0, 5, 3)
        .expect("must parse");
    assert!(chunks.len() > 3, "expected a split window: {}", chunks.len());

    let idx: Vec<u64> = chunks
        .iter()
        .map(|c| c.metadata["window_index"].as_u64().expect("window_index"))
        .collect();

    // 0-based, contiguous, non-decreasing.
    assert_eq!(idx[0], 0, "indices must start at 0: {idx:?}");
    for w in idx.windows(2) {
        assert!(
            w[1] == w[0] || w[1] == w[0] + 1,
            "indices must be contiguous and non-decreasing: {idx:?}"
        );
    }
    // Three windows: [p0,p1] [p2,p3] [p4].
    let distinct: std::collections::BTreeSet<u64> = idx.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        3,
        "window_index must count WINDOWS (3), not emitted chunks ({}): {idx:?}",
        chunks.len()
    );
    // The split window's parts share an index.
    assert!(
        idx.iter().filter(|&&i| i == 1).count() >= 2,
        "the size-split window's parts must share window_index 1: {idx:?}"
    );
    // And the empty-chunk fix holds alongside.
    assert!(
        chunks.iter().all(|c| !c.content.trim().is_empty()),
        "empty chunk leaked"
    );
}
