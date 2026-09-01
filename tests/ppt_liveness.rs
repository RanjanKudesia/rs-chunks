//! Only LIVE records may reach `.ppt` output.
//!
//! A `.ppt` is edited in place: every save appends records, and the persist
//! directory reached from `Current User` is the only map of what is current.
//! The reader was a linear scanner with no liveness layer, so it emitted
//! deleted slides as live text and counted every save's slide list —
//! `poi_47261.ppt` reported `total_slides` 305 for a 14-slide deck. It also
//! emitted title masters (live records reachable only via the master list)
//! as slides, which no record-type filter can prevent.
//!
//! Acceptance numbers were derived from raw bytes across all 28 fixtures
//! BEFORE implementation (spec_ppt_persist.md) — the asserts below are that
//! derivation, not the implementation's own output blessed after the fact.

use chunks_rs::formats::ppt;

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/ppt")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p.to_str().unwrap().to_string()
}

fn total_slides(name: &str) -> u64 {
    let chunks = ppt::chunk(&fixture(name), "structural", 3, 1, 5, 3).expect("must parse");
    chunks[0].metadata["document_metadata"]["total_slides"]
        .as_u64()
        .expect("total_slides")
}

#[test]
fn a_multi_save_deck_reports_live_slides_not_every_saves_list() {
    // 22 saves × one SlideListWithText each summed to 305; live = 14.
    assert_eq!(total_slides("poi_47261.ppt"), 14, "dead slide lists counted");
}

#[test]
fn deleted_slide_text_is_not_emitted() {
    // poi_bullets.ppt: 6 saves, 2 live slides. The five ghost paragraphs
    // below exist only in superseded records.
    assert_eq!(total_slides("poi_bullets.ppt"), 2);
    let md = ppt::to_markdown(&fixture("poi_bullets.ppt")).expect("must parse");
    for ghost in ["First line", "Second line", "Third line"] {
        assert!(
            !md.contains(ghost),
            "deleted content emitted as live: {ghost:?} in {md:?}"
        );
    }
}

#[test]
fn title_masters_are_not_slides() {
    // Single-save decks where the stream holds one more SlideContainer than
    // the live list references — a title master. Live counts from raw bytes.
    assert_eq!(total_slides("sample1.ppt"), 19, "title master counted");
    assert_eq!(total_slides("poi_49541_symbol_map.ppt"), 1);
}

#[test]
fn single_save_guards_are_unchanged() {
    // Spec §4.3: 1 edit, 0 title masters, 0 dead containers — the fix must
    // change NOTHING here.
    assert_eq!(total_slides("sample3.ppt"), 17);
    assert_eq!(total_slides("poi_customGeo.ppt"), 48);
    assert_eq!(total_slides("poi_br_tvcamboriu_pensar.ppt"), 14);
}
