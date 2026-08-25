//! An empty document is not an error — and every mode must agree (TECH_DEBT T6).
//!
//! The contract is one sentence: a document that **parsed fine but has nothing
//! to chunk** returns `[]`; only a structurally invalid document raises. That
//! rule had landed for `default` and `structural` and nowhere else, so whether
//! a caller saw an exception depended on which mode they happened to ask for —
//! the same defect T6 was opened for, still live in five modes out of seven.
//!
//! `.csv` and `.tsv` were worse than inconsistent, they disagreed with *each
//! other* on identical bytes: `.csv` raised "CSV file is empty" while `.tsv`
//! returned `[]`, and whitespace-only `.tsv` returned one chunk containing
//! nothing but a column label. The cause was not the delimiter treating
//! whitespace as data — it was that supplying a delimiter skipped the check
//! entirely. `.tsv` passes `Some(b'\t')`, `.csv` passes `None` and fell into
//! auto-detection, whose "no line to sniff" failure was reported as an
//! emptiness error.
//!
//! `.json` deliberately still raises: an empty file is *invalid JSON*, which is
//! the error branch of the same rule, not an exception to it.

use chunks_rs::formats::{csv, html, md, txt};

const MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

/// Delimited formats expose four of the seven modes.
const DELIM_MODES: &[&str] = &["row", "default", "sliding_window", "page_aware"];

/// Empty, whitespace-only, and BOM-only. The third matters because `decode_text`
/// strips the BOM, so the document becomes `""` only *after* decoding.
const EMPTY_INPUTS: &[(&str, &[u8])] = &[
    ("empty", b""),
    ("spaces", b"   "),
    ("newlines", b"\n\n\n"),
    ("mixed whitespace", b" \t\r\n  \n"),
    ("utf-8 BOM only", b"\xEF\xBB\xBF"),
];

#[test]
fn md_returns_empty_from_every_mode() {
    for (label, bytes) in EMPTY_INPUTS {
        for mode in MODES {
            let got = md::chunk_from_bytes(bytes, mode, 3, 1, 3, 15);
            let chunks = got
                .unwrap_or_else(|e| panic!("md {mode} on {label}: must not raise, got {e:?}"));
            assert!(chunks.is_empty(), "md {mode} on {label}: expected [], got {chunks:?}");
        }
    }
}

#[test]
fn txt_returns_empty_from_every_mode() {
    for (label, bytes) in EMPTY_INPUTS {
        for mode in MODES {
            let got = txt::chunk_from_bytes(bytes, mode, 3, 1, 3, 15);
            let chunks = got
                .unwrap_or_else(|e| panic!("txt {mode} on {label}: must not raise, got {e:?}"));
            assert!(chunks.is_empty(), "txt {mode} on {label}: expected [], got {chunks:?}");
        }
    }
}

#[test]
fn html_returns_empty_from_every_mode() {
    for (label, bytes) in EMPTY_INPUTS {
        for mode in MODES {
            let got = html::chunk_from_bytes(bytes, mode, 3, 1, 3, 15);
            let chunks = got
                .unwrap_or_else(|e| panic!("html {mode} on {label}: must not raise, got {e:?}"));
            assert!(chunks.is_empty(), "html {mode} on {label}: expected [], got {chunks:?}");
        }
    }
}

/// Tag-only HTML reaches `[]` by the *other* route — the source is not blank, so
/// the head guard does not fire and the block walker simply yields nothing.
/// Both routes must agree.
#[test]
fn html_with_only_markup_returns_empty_from_every_mode() {
    let bytes = b"<html><head><title></title></head><body><div>  </div><span></span></body></html>";
    for mode in MODES {
        let got = html::chunk_from_bytes(bytes, mode, 3, 1, 3, 15);
        let chunks =
            got.unwrap_or_else(|e| panic!("html {mode} on markup-only: must not raise, got {e:?}"));
        assert!(chunks.is_empty(), "html {mode} on markup-only: expected [], got {chunks:?}");
    }
}

/// The one that used to disagree with itself. `.csv` and `.tsv` are the same
/// code path — the only difference is that `.tsv` supplies its delimiter.
#[test]
fn csv_and_tsv_agree_on_empty_input() {
    for (label, bytes) in EMPTY_INPUTS {
        for mode in DELIM_MODES {
            let as_csv = csv::chunk_from_bytes(bytes, mode, 10, 3, 1, true, None, "auto", true);
            let as_tsv =
                csv::chunk_from_bytes(bytes, mode, 10, 3, 1, true, Some(b'\t'), "auto", true);

            let csv_chunks = as_csv
                .unwrap_or_else(|e| panic!("csv {mode} on {label}: must not raise, got {e:?}"));
            let tsv_chunks = as_tsv
                .unwrap_or_else(|e| panic!("tsv {mode} on {label}: must not raise, got {e:?}"));

            assert!(csv_chunks.is_empty(), "csv {mode} on {label}: expected [], got {csv_chunks:?}");
            assert!(tsv_chunks.is_empty(), "tsv {mode} on {label}: expected [], got {tsv_chunks:?}");
        }
    }
}

/// A file of nothing but comments has no data line to sniff a delimiter from.
/// That is the same "detection failed" path, and it must not raise either.
#[test]
fn csv_with_only_comments_returns_empty() {
    let bytes = b"# just a comment\n# and another\n";
    for mode in DELIM_MODES {
        let got = csv::chunk_from_bytes(bytes, mode, 10, 3, 1, true, None, "auto", true);
        let chunks =
            got.unwrap_or_else(|e| panic!("csv {mode} on comments-only: must not raise, got {e:?}"));
        assert!(chunks.is_empty(), "csv {mode} on comments-only: expected [], got {chunks:?}");
    }
}
