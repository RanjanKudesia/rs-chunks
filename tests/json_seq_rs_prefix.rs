//! An RFC 7464 record separator must not reach chunk content.
//!
//! `application/json-seq` prefixes each record with RS (U+001E), and such files
//! are routinely named `.ndjson`. `str::trim` does not strip RS — it is not
//! Unicode White_Space — so before the fix the byte survived the trim,
//! `serde_json` rejected the line, and `parse_lines` pushed the raw source as
//! the record. Three compounding failures from one input: a raw control byte in
//! `content`, 100% loss of key/value rendering, and no signal of either.

use chunks_rs::formats::json;

#[test]
fn an_rs_prefixed_record_is_parsed_not_dumped() {
    let raw = "\u{1e}{\"id\":1,\"msg\":\"alpha\"}\n\u{1e}{\"id\":2,\"msg\":\"beta\"}\n";
    let md = json::to_markdown_from_bytes(raw.as_bytes(), "seq.ndjson")
        .expect("a line-oriented json path does not fail");

    assert!(
        !md.contains('\u{1e}'),
        "raw RS (U+001E) reached the output: {md:?}"
    );
    // Parsed, not dumped: the rendered form carries the key, the raw form does not.
    assert!(
        md.contains("alpha") && md.contains("beta"),
        "records went missing: {md:?}"
    );
    assert!(
        !md.contains("{\"id\":1"),
        "the record was emitted as raw source instead of being parsed: {md:?}"
    );
}

/// Control: a plain NDJSON file must be unaffected by the RS strip.
#[test]
fn a_plain_ndjson_record_is_unchanged() {
    let raw = "{\"id\":1,\"msg\":\"alpha\"}\n{\"id\":2,\"msg\":\"beta\"}\n";
    let md = json::to_markdown_from_bytes(raw.as_bytes(), "plain.ndjson")
        .expect("a line-oriented json path does not fail");
    assert!(md.contains("alpha") && md.contains("beta"), "{md:?}");
    assert!(
        !md.contains("{\"id\":1"),
        "should be rendered, not raw: {md:?}"
    );
}
