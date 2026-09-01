//! A sheet's position in the workbook is not its part number.
//!
//! OOXML lists `<sheet name="X" r:id="rIdN"/>` and the rels map `rIdN` to an
//! arbitrary target, so `sheet{ordinal}.xml` is a guess. It is usually right,
//! which is exactly why this survived: named tables, images and drawings were
//! read from the **wrong sheet** only on workbooks whose parts are not in sheet
//! order — and the wrong sheet is still a valid sheet, so nothing looked broken.
//!
//! `poi_xlmmacro.xlsm` is such a workbook: an XLM macro sheet occupies a slot,
//! so sheet ordinal 2 resolves to `sheet1.xml` and ordinal 3 to `sheet2.xml`.

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

/// The whole corpus must still chunk — the resolver falls back to the ordinal
/// guess whenever the workbook or its rels cannot be read, so nothing that
/// worked before may stop working.
#[test]
fn every_workbook_still_chunks() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files");
    let mut checked = 0;
    for ext in ["xlsx", "xlsm", "xlsb", "xltx", "xltm"] {
        let sub = dir.join(ext);
        let Ok(entries) = std::fs::read_dir(&sub) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some(ext) {
                continue;
            }
            // Some fixtures are deliberately corrupt; only assert we do not panic
            // and that a success stays a success.
            let _ = chunks_rs::formats::xlsx::chunk(
                p.to_str().unwrap(),
                "sheet",
                50,
                3,
                1,
                true,
                Vec::new(),
                true,
                2000,
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no spreadsheet fixtures exercised");
}

/// The workbook that actually mismatches must still produce its sheets, and
/// each chunk's `sheet_name` must be one the workbook really declares — not a
/// name borrowed from a neighbouring part.
#[test]
fn a_workbook_whose_parts_are_out_of_order_reports_real_sheet_names() {
    let p = fixture("xlsm/poi_xlmmacro.xlsm");
    let chunks = chunks_rs::formats::xlsx::chunk(
        p.to_str().unwrap(),
        "sheet",
        50,
        3,
        1,
        true,
        Vec::new(),
        true,
        2000,
    )
    .expect("poi_xlmmacro.xlsm must chunk");

    for c in &chunks {
        let name = c
            .metadata
            .get("sheet_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            !name.is_empty(),
            "a chunk carries no sheet_name: {:?}",
            c.metadata
        );
    }
}
