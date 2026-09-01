//! Envelope sibling keys are content, not packaging.
//!
//! The envelope heuristic picks `features`/`data`/… as the record list — and
//! silently DELETED every sibling: `vega_earthquakes.json` has top-level
//! `type`, `metadata` (title + source URL) and `bbox` beside `features`, and
//! `type`'s value appeared nowhere in 1,526,785 characters of output. The
//! siblings now render as a preamble ahead of the records.

use chunks_rs::formats::json;

#[test]
fn geojson_envelope_siblings_reach_the_output() {
    let doc = br#"{"type":"FeatureCollection","bbox":[1,2,3,4],
        "features":[{"id":"AFG"},{"id":"ALB"}]}"#;
    let md = json::to_markdown_from_bytes(doc, "d.json").expect("must parse");
    assert!(
        md.contains("FeatureCollection"),
        "envelope sibling `type` was deleted: {md:?}"
    );
    assert!(md.contains("bbox"), "envelope sibling `bbox` deleted: {md:?}");
    assert!(md.contains("AFG") && md.contains("ALB"), "records lost: {md:?}");
}

/// Control: a bare array document has no envelope and must be unchanged.
#[test]
fn a_bare_array_is_unchanged() {
    let doc = br#"[{"id":1},{"id":2}]"#;
    let md = json::to_markdown_from_bytes(doc, "d.json").expect("must parse");
    assert!(md.contains("id"), "{md:?}");
}
