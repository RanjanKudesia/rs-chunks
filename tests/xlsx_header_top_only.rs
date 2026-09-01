//! A header row can only be at the top; rows above a "header" are data.
//!
//! `detect_header_row` scanned the whole sheet for the first header-shaped
//! row, and every caller drops the rows above it. `poi_46535.xlsx` sheet
//! `Others` holds 331 rows of `#REF!` error cells with an all-string row at
//! 332: the engine emitted **2 chunks for 331 rows**, `skipped_sheets: []` —
//! the worst silent loss the format review found, and structurally invisible
//! to the golden snapshot (a shrunken sheet just hashes to a stable wrong
//! value). The first non-empty row now decides.

use chunks_rs::formats::xlsx;

#[test]
fn rows_above_a_deep_header_shaped_row_are_not_deleted() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/excel/poi_46535.xlsx");
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    let chunks =
        xlsx::chunk(p.to_str().unwrap(), "row", 1, 1, 0, true, Vec::new(), false, 1200)
            .expect("must parse");
    let others = chunks
        .iter()
        .filter(|c| c.metadata["sheet_name"] == "Others")
        .count();
    assert!(
        others >= 300,
        "sheet `Others` has 331 rows; engine emitted {others}"
    );
    // The healthy sheets must be untouched by the heuristic change.
    let codelists = chunks
        .iter()
        .filter(|c| c.metadata["sheet_name"] == "CodeLists")
        .count();
    assert!(
        (2430..=2439).contains(&codelists),
        "CodeLists regressed: {codelists}"
    );
}
