//! A blank table cell is a position, not noise.
//!
//! `slide_xml` filtered empty cells out of each row before joining, so every
//! column right of a gap shifted one place left and its value landed under the
//! wrong header. `get_markdown` keeps blanks, so the two surfaces disagreed on
//! the same deck — and the chunk surface, the one that feeds retrieval, was the
//! wrong one.
//!
//! Measured on a real calendar deck: February 2006 starts on a Wednesday.
//! `get_markdown` rendered `|  |  |  | 1 | 2 | 3 | 4 |`; `get_chunks` rendered
//! `1 | 2 | 3 | 4`, filing the 1st under SUNDAY.

use std::path::{Path, PathBuf};

fn fixture(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .join("test_files")
        .join(rel);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

#[test]
fn a_leading_blank_cell_keeps_the_column_alignment() {
    let p = fixture("potx/oxml_03_2006Calendar_TP10081921.potx");
    let chunks = chunks_rs::formats::pptx::chunk(p.to_str().unwrap(), "structural", 3, 1, 3, 15)
        .expect("the calendar deck must chunk");

    let february = chunks
        .iter()
        .map(|c| c.content.as_str())
        .find(|c| c.contains("February"))
        .expect("no February slide found");

    // Seven day columns, and the 1st sits in the fourth — Wednesday.
    let first_week = february
        .lines()
        .find(|l| l.trim_start().starts_with("|") || l.contains(" 1 | 2 | 3 | 4"))
        .unwrap_or("");
    assert!(
        february.contains(" |  | 1 | 2 | 3 | 4") || first_week.contains(" 1 | 2 | 3 | 4"),
        "February's first week lost its leading blanks: {february:?}"
    );
    assert!(
        !february.contains("\n1 | 2 | 3 | 4"),
        "the 1st is still filed under SUNDAY — blanks were dropped: {february:?}"
    );
}

/// Whatever the chunk surface does with a row, the number of columns must match
/// what the markdown surface renders. This is the invariant, not the symptom.
#[test]
fn chunk_and_markdown_rows_have_the_same_column_count() {
    let p = fixture("potx/oxml_03_2006Calendar_TP10081921.potx");
    let path = p.to_str().unwrap();
    let chunks = chunks_rs::formats::pptx::chunk(path, "structural", 3, 1, 3, 15).unwrap();
    let md = chunks_rs::formats::pptx::to_markdown(path).unwrap();

    let chunk_row = chunks
        .iter()
        .flat_map(|c| c.content.lines())
        .find(|l| l.contains("1 | 2 | 3 | 4"))
        .expect("no first-week row in chunks");
    let md_row = md
        .lines()
        .find(|l| l.contains("1 | 2 | 3 | 4"))
        .expect("no first-week row in markdown");

    let chunk_cols = chunk_row.split('|').count();
    // Markdown wraps rows in leading/trailing pipes, so it has two extra fields.
    let md_cols = md_row.split('|').count() - 2;
    assert_eq!(
        chunk_cols, md_cols,
        "column counts disagree:\n  chunks:   {chunk_row:?}\n  markdown: {md_row:?}"
    );
}

/// The space before an XML entity must survive on BOTH surfaces.
///
/// `md_slide_parse` trimmed the first text event of an `<a:t>` and appended the
/// entity-spill events verbatim, so `<a:t>O'Reilly &amp; Associates</a:t>`
/// arrived as `"O'Reilly "`, `"&"`, `" Associates"` and the trailing space of
/// the first segment was eaten. `slide_xml` buffers the whole element and trims
/// once, which is right — so the same deck read one way through `get_chunks`
/// and another through `get_markdown`.
#[test]
fn an_entity_keeps_the_space_before_it_on_both_surfaces() {
    let p = fixture("pptx/poi_2411-Performance_Up.pptx");
    let path = p.to_str().unwrap();

    let md = chunks_rs::formats::pptx::to_markdown(path).expect("markdown");
    let chunks = chunks_rs::formats::pptx::chunk(path, "structural", 3, 1, 3, 15).expect("chunks");
    let chunk_text: String = chunks.iter().map(|c| c.content.as_str()).collect();

    for (surface, body) in [("markdown", &md), ("chunks", &chunk_text)] {
        assert!(
            body.contains("O\u{2019}Reilly & Associates"),
            "{surface}: the space before the entity was eaten: {:?}",
            body.split("Associates")
                .next()
                .map(|s| &s[s.len().saturating_sub(40)..])
        );
        assert!(
            !body.contains("O\u{2019}Reilly& Associates"),
            "{surface}: the eaten-space shape is back"
        );
    }
}
