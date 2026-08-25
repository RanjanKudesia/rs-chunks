//! A 522,261-character single chunk is not a chunk (TECH_DEBT F5, inert half).
//!
//! Two unbounded sites in docx, both reachable only from pathological
//! documents — the largest table in an ordinary corpus file is 1,737 chars and
//! the largest window 3,552, against a 6,000 cap. The measurement gap between
//! those and the smallest affected fixture (39,673) is empty, which is what
//! makes this half of F5 safe to land without re-baselining ordinary output.
//!
//! The two bounds are deliberately different in kind:
//!
//! * **Tables are split on ROW boundaries only.** `split_block_on_lines` never
//!   breaks a line, so no row is ever halved — the same guarantee that protects
//!   a CSV record. That makes it a *soft* bound: a single row longer than the
//!   cap still comes out over it, because halving a row corrupts the record and
//!   an over-long row is the lesser harm.
//! * **Windows are split on word boundaries.** On that path a table's newlines
//!   have already been collapsed to spaces, so it arrives as one line and the
//!   line splitter alone would not touch it. That one needs a real hard bound.

use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .join("test_files/docx")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

fn max_chunk(name: &str, mode: &str) -> usize {
    let p = fixture(name);
    chunks_rs::formats::docx::chunk(p.to_str().unwrap(), mode, 3, 1, 3, 15)
        .unwrap_or_else(|e| panic!("{name} [{mode}]: {e}"))
        .iter()
        .map(|c| c.content.chars().count())
        .max()
        .unwrap_or(0)
}

/// The window bound is hard: nothing may exceed it, on any fixture.
#[test]
fn no_sliding_window_chunk_exceeds_the_cap() {
    for name in [
        "poi_bug65649.docx",
        "poi_bug59058.docx",
        "_stress_big_table.docx",
        "poi_drawing.docx",
    ] {
        let max = max_chunk(name, "sliding_window");
        assert!(
            max <= 6_000,
            "{name}: a sliding_window chunk is {max} chars, over the 6,000 cap"
        );
    }
}

/// The table bound is soft, and the softness is the point — but it must still
/// have cut the 522,261-char monster down by more than an order of magnitude.
#[test]
fn an_enormous_table_is_split_into_rows() {
    let max = max_chunk("poi_bug65649.docx", "structural");
    assert!(
        max < 50_000,
        "the 522,261-char table was not split at all: max is {max}"
    );
    assert!(
        max > 6_000,
        "max is {max} — if this is under the cap the split stopped honouring \
         row boundaries, which would mean rows are being halved"
    );
}

/// The documented residue: one cell holding 93,024 chars is ONE line, and
/// splitting it would halve a table row. It is correct for this to stay over
/// the cap, and a future change that "fixes" it has broken the row guarantee.
#[test]
fn a_single_huge_cell_is_left_intact() {
    let max = max_chunk("poi_bug59058.docx", "structural");
    assert!(
        max > 50_000,
        "a single 93k-char cell was split, halving a table row: max is {max}"
    );
}

/// Ordinary documents must be untouched — this is what keeps the change inert.
#[test]
fn an_ordinary_document_is_unchanged() {
    let max = max_chunk("poi_drawing.docx", "structural");
    assert!(
        max < 6_000,
        "an ordinary document is near the cap ({max}); the headroom this change \
         relies on has gone"
    );
}
