//! JSON-family encoding: a BOM is not content, and a non-UTF-8 file is not
//! invalid JSON (TECH_DEBT C4).
//!
//! RFC 8259 §8.1 makes UTF-8 the JSON encoding and says a BOM is not part of
//! the text — but `serde_json` sees the BOM as a stray codepoint and rejects the
//! file as *"expected value at line 1 column 1"*, an error naming neither the
//! cause nor the remedy. A Windows-exported `.json` is an entirely ordinary
//! file and was unreadable.
//!
//! These paths take the ladder at the **byte** level, not the text level:
//! newline normalisation must not run, because a `\r\n` inside a string literal
//! is data rather than a line ending.

use chunks_rs::formats::{ipynb, json};

const JSON_MODES: &[&str] = &["default", "structural", "section", "semantic"];

fn bom(body: &str) -> Vec<u8> {
    let mut v = vec![0xEF, 0xBB, 0xBF];
    v.extend_from_slice(body.as_bytes());
    v
}

#[test]
fn a_bom_does_not_make_json_invalid() {
    let body = r#"[{"id":1,"name":"first record here"},{"id":2,"name":"second record here"}]"#;
    for mode in JSON_MODES {
        let with = json::chunk_from_bytes(&bom(body), "doc.json", mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("[{mode}] BOM'd json must parse, got {e}"));
        let without = json::chunk_from_bytes(body.as_bytes(), "doc.json", mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("[{mode}] plain json: {e}"));
        let a: Vec<&str> = with.iter().map(|c| c.content.as_str()).collect();
        let b: Vec<&str> = without.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(a, b, "[{mode}] the BOM changed the parse");
    }
}

/// The newline caveat: `\r\n` inside a string literal is data. If this path ever
/// normalised newlines before parsing it would silently rewrite the value.
#[test]
fn crlf_inside_a_json_string_is_preserved() {
    let body = r#"[{"text":"line one\r\nline two, long enough to survive"}]"#;
    let chunks = json::chunk_from_bytes(body.as_bytes(), "doc.json", "structural", 3, 1, 3, 15)
        .expect("crlf json");
    let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
    assert!(
        joined.contains("line one") && joined.contains("line two"),
        "the string value was mangled: {joined:?}"
    );
}

/// `.jsonl` used to emit record 1 as a raw unparseable paragraph when a BOM was
/// present, because the BOM was prepended to the first line.
#[test]
fn a_bom_does_not_break_the_first_jsonl_record() {
    let body = "{\"a\":\"first value here\"}\n{\"a\":\"second value here\"}\n";
    for mode in JSON_MODES {
        let with = json::chunk_from_bytes(&bom(body), "doc.jsonl", mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("[{mode}] BOM'd jsonl: {e}"));
        let without = json::chunk_from_bytes(body.as_bytes(), "doc.jsonl", mode, 3, 1, 3, 15)
            .unwrap_or_else(|e| panic!("[{mode}] plain jsonl: {e}"));
        let a: Vec<&str> = with.iter().map(|c| c.content.as_str()).collect();
        let b: Vec<&str> = without.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(a, b, "[{mode}] the BOM changed the records");
        assert!(
            !a.iter().any(|c| c.contains('\u{FEFF}')),
            "[{mode}] the BOM leaked into a chunk: {a:?}"
        );
    }
}

/// A notebook is JSON and inherits the same handling.
#[test]
fn a_bom_does_not_make_a_notebook_invalid() {
    let nb = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[
        {"cell_type":"markdown","metadata":{},"source":["# Title\n","\n","Some prose in the notebook.\n"]}
    ]}"##;
    let with = ipynb::chunk_from_bytes(&bom(nb), "structural", 3, 1, 3, 15)
        .expect("BOM'd notebook must parse");
    let without =
        ipynb::chunk_from_bytes(nb.as_bytes(), "structural", 3, 1, 3, 15).expect("plain notebook");
    let a: Vec<&str> = with.iter().map(|c| c.content.as_str()).collect();
    let b: Vec<&str> = without.iter().map(|c| c.content.as_str()).collect();
    assert_eq!(a, b, "the BOM changed the notebook parse");
}
