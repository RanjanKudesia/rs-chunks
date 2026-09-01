//! Object members must render in document order, not alphabetical order.
//!
//! `serde_json` defaults to a `BTreeMap`, which sorts keys. So
//! `{"name":"Andy","age":30}` rendered as `age` before `name`. For `.json` that
//! silently reorders every object; for `.jsonl`/`.ndjson` — whose dominant real
//! use is logs — it scrambles the causal order of every record, putting
//! `timestamp` last in an 8-key nginx log line across tens of thousands of
//! chunks.
//!
//! The fix is one feature flag (`preserve_order`) in Cargo.toml, which switches
//! the backing map to an order-preserving one.

use chunks_rs::formats::json;

#[test]
fn json_object_members_keep_document_order() {
    let doc = br#"{"zulu":1,"alpha":2,"mike":3}"#;
    let md = json::to_markdown_from_bytes(doc, "d.json").expect("must parse");
    let (z, a, m) = (
        md.find("zulu").expect("zulu present"),
        md.find("alpha").expect("alpha present"),
        md.find("mike").expect("mike present"),
    );
    assert!(
        z < a && a < m,
        "members were alphabetised rather than kept in document order: {md:?}"
    );
}

#[test]
fn jsonl_records_keep_document_order() {
    let doc = b"{\"name\":\"Andy\",\"age\":30}\n{\"name\":\"Bea\",\"age\":41}\n";
    let md = json::to_markdown_from_bytes(doc, "d.jsonl").expect("must parse");
    let (n, a) = (
        md.find("Andy").expect("name value present"),
        md.find("30").expect("age value present"),
    );
    assert!(
        n < a,
        "`age` was hoisted above `name` by alphabetisation: {md:?}"
    );
}
