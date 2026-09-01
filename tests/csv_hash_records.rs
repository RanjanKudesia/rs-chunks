//! A record whose first byte is `#` is data, not a comment.
//!
//! All three csv entry points passed `.comment(Some(b'#'))` to the reader, so a
//! record beginning `#` was silently DELETED — no error, no warning, and in
//! `row` mode `actual_row_count` is `None`, so nothing downstream could even
//! detect that the row count was short. The same character mid-field survived,
//! which is what made it hard to spot.
//!
//! Neither RFC 4180 nor the `text/tab-separated-values` registration defines a
//! comment convention. `#` is TEXTDATA. Any file whose first column carries
//! issue numbers (`#4`), SKUs, hex colours or hashtags lost exactly those rows.

use chunks_rs::formats::csv;

const WITH_HASH: &str = "id,item,note\n\
                         1,widget,ok\n\
                         2,#hashtag campaign,live\n\
                         #4,legacy sku,archived\n\
                         3,gadget,ok\n";

#[test]
fn a_record_beginning_with_hash_is_kept() {
    let md = csv::to_markdown_from_bytes(WITH_HASH.as_bytes(), None, "auto")
        .expect("well-formed csv must parse");
    assert!(
        md.contains("legacy sku"),
        "the `#4` record was deleted as a comment: {md:?}"
    );
}

#[test]
fn a_hash_mid_field_is_unaffected() {
    let md = csv::to_markdown_from_bytes(WITH_HASH.as_bytes(), None, "auto")
        .expect("well-formed csv must parse");
    assert!(
        md.contains("#hashtag campaign"),
        "mid-field `#` was disturbed: {md:?}"
    );
}

/// Every data row must survive — the count is the point, not any one row.
#[test]
fn no_row_is_silently_dropped() {
    let chunks = csv::chunk_from_bytes(
        WITH_HASH.as_bytes(),
        "row",
        1,     // rows_per_chunk
        1,     // window_size
        0,     // overlap
        true,  // include_headers
        None,  // delimiter -> sniff
        "auto",
        false, // skip_empty_rows
    )
    .expect("well-formed csv must parse");
    let all: String = chunks.iter().map(|c| c.content.as_str()).collect();
    for needle in ["widget", "hashtag campaign", "legacy sku", "gadget"] {
        assert!(all.contains(needle), "row {needle:?} vanished: {all:?}");
    }
}

/// The same rule for `.tsv`, which shares this module and supplies a delimiter.
#[test]
fn tsv_keeps_hash_records_too() {
    let tsv = "id\tname\n1\talpha\n#2\tbeta\n";
    let md = csv::to_markdown_from_bytes(tsv.as_bytes(), Some(b'\t'), "auto")
        .expect("well-formed tsv must parse");
    assert!(md.contains("beta"), "the `#2` tsv record was deleted: {md:?}");
}
