//! ODF table cells are positions, and a nested table belongs in its parent.
//!
//! Two defects at one site:
//!
//! * An empty cell is written `<table:table-cell/>` — an `Empty` event, not
//!   `Start`+`End` — so it never reached the cell handling and no cell was
//!   pushed. Every later cell in the row shifted left into the wrong column.
//!   Measured on `odftoolkit_Presentation2.odp`: a declared 3-column table
//!   rendered as one column.
//! * A nested `table:table` **overwrote** the outer one, so the outer table's
//!   completed rows were dropped, the inner table was emitted outside its
//!   parent cell, and every remaining outer cell leaked into body text.

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

/// The declared column count must survive an empty leading cell.
#[test]
fn an_empty_cell_holds_its_column() {
    let p = fixture("odp/odftoolkit_Presentation2.odp");
    let md = chunks_rs::formats::odf::to_markdown(p.to_str().unwrap()).expect("must parse");

    let table_rows: Vec<&str> = md
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect();
    assert!(!table_rows.is_empty(), "no table rendered at all: {md:?}");

    for row in &table_rows {
        let cols = row.split('|').count();
        assert_eq!(
            cols, 5,
            "row has {} fields, expected 5 (3 columns plus the outer pipes): {row:?}",
            cols
        );
    }
    assert!(
        table_rows.iter().any(|r| r.starts_with("|  | ddd")),
        "the second row's value is not in column 2: {table_rows:?}"
    );
}

/// A nested table is flattened into the parent cell; the outer table survives.
#[test]
fn a_nested_table_does_not_replace_its_parent() {
    let content = r#"<?xml version="1.0"?>
<office:document-content
  xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
  xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
  xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
 <office:body><office:text>
  <table:table>
   <table:table-row>
    <table:table-cell><text:p>outer-a</text:p>
      <table:table>
       <table:table-row><table:table-cell><text:p>inner-1</text:p></table:table-cell>
        <table:table-cell><text:p>inner-2</text:p></table:table-cell></table:table-row>
      </table:table>
    </table:table-cell>
    <table:table-cell><text:p>outer-b</text:p></table:table-cell>
   </table:table-row>
   <table:table-row>
    <table:table-cell><text:p>outer-c</text:p></table:table-cell>
    <table:table-cell><text:p>outer-d</text:p></table:table-cell>
   </table:table-row>
  </table:table>
 </office:text></office:body>
</office:document-content>"#;

    let odt = build_odt(content);
    let md = chunks_rs::formats::odf::to_markdown_from_bytes(&odt, "nested.odt")
        .expect("nested table must parse");

    for needle in ["outer-a", "outer-b", "outer-c", "outer-d", "inner-1", "inner-2"] {
        assert!(md.contains(needle), "lost {needle}: {md:?}");
    }
    // The inner table must be inside the parent cell, not a block of its own.
    assert!(
        md.lines().filter(|l| l.contains("---")).count() == 1,
        "the nested table was emitted as its own table: {md:?}"
    );
}

/// Minimal ODT zip carrying just the mimetype and content.xml.
fn build_odt(content_xml: &str) -> Vec<u8> {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/vnd.oasis.opendocument.text")
            .unwrap();
        zip.start_file("content.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}
