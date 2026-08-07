//! Adversarial malformed-input tests — **fixture-free** (inputs are
//! synthesised in-memory), so this file also runs on CI where the
//! `../test_files` corpus does not exist (see .github/workflows/release.yml).
//!
//! The contract under test: corrupt input must produce a clean `Err` (or a
//! well-formed `Ok`) — never a panic escaping the public dispatch API. The
//! dispatch layer's `catch_unwind` boundary converts any internal parser panic
//! into `ChunkError::Parse` (see `src/dispatch.rs`), and its unit tests prove
//! the conversion; these tests prove real corrupt bytes come out clean.

use chunks_rs::{
    get_chunks_from_bytes, get_chunks_with_images_from_bytes, get_markdown_from_bytes,
};

/// Deterministic pseudo-random bytes (xorshift) — "binary garbage" that is
/// stable across runs so a failure is reproducible.
fn garbage(len: usize, mut seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.push((seed & 0xFF) as u8);
    }
    out
}

/// Run every dispatch surface over the bytes; the assertion is simply that we
/// get *back* — a panic would abort the test process, an unwind would fail the
/// test. Errors must render a non-empty message.
fn assert_no_panic(data: &[u8], filename: &str) {
    for mode in ["default", "semantic", "sentence", "sliding_window"] {
        if let Err(e) = get_chunks_from_bytes(data, filename, mode, 3, 1, 3, 15) {
            assert!(!e.to_string().is_empty(), "empty error message for {filename} ({mode})");
        }
    }
    if let Err(e) = get_markdown_from_bytes(data, filename) {
        assert!(!e.to_string().is_empty(), "empty markdown error message for {filename}");
    }
    if let Err(e) = get_chunks_with_images_from_bytes(data, filename, "default", 3, 1, 3, 15) {
        assert!(!e.to_string().is_empty(), "empty images error message for {filename}");
    }
}

// ── eml: the two adversarial cases the tracker calls out explicitly ─────────

#[test]
fn eml_truncated_mime_fails_cleanly() {
    // A multipart message chopped mid-base64, mid-part: no closing boundary,
    // no terminating "--", dangling headers.
    let truncated = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: truncated multipart\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"XYZ\"\r\n\
\r\n\
--XYZ\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Body text that never ends because the message is cut\r\n\
--XYZ\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
SGVsbG8gd29ybGQgdGhpcyBpcyBiYXNlNjQgZGF0YSB0aGF0IGp1c3Qgc3RvcHM";
    assert_no_panic(truncated, "truncated.eml");

    // Progressive truncation: every prefix must also come back cleanly.
    for cut in [10, 50, 100, 200, truncated.len() - 1] {
        assert_no_panic(&truncated[..cut], "cut.eml");
    }
}

#[test]
fn eml_binary_garbage_fails_cleanly() {
    assert_no_panic(&garbage(4096, 0xDEADBEEF), "garbage.eml");
    // Garbage that *starts* like a header, then degenerates.
    let mut half = b"Received: from mail.example.com\r\nContent-Type: ".to_vec();
    half.extend(garbage(2048, 42));
    assert_no_panic(&half, "half-garbage.eml");
    // NUL-ridden and empty inputs.
    assert_no_panic(&[0u8; 512], "nuls.eml");
    assert_no_panic(b"", "empty.eml");
}

#[test]
fn mbox_garbage_fails_cleanly() {
    assert_no_panic(&garbage(4096, 7), "garbage.mbox");
    let mut half = b"From alice@example.com Thu Jan  1 00:00:00 2026\r\n".to_vec();
    half.extend(garbage(1024, 8));
    assert_no_panic(&half, "half-garbage.mbox");
}

// ── every other family: garbage + truncated-container inputs ────────────────

#[test]
fn garbage_bytes_never_panic_any_family() {
    let all = [
        "csv", "tsv", "md", "txt", "html", "json", "jsonl", "ndjson", "odt", "odp", "msg",
        "ipynb", "rtf", "pdf", "epub", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "ods",
    ];
    let junk = garbage(2048, 0xC0FFEE);
    for ext in all {
        assert_no_panic(&junk, &format!("junk.{ext}"));
        assert_no_panic(b"", &format!("empty.{ext}"));
    }
}

#[test]
fn truncated_containers_fail_cleanly() {
    // A real (tiny) ZIP local-file header that stops mid-record — feeds the
    // OOXML/ODF/EPUB zip paths a structurally plausible but impossible file.
    let mut zipish = vec![0x50, 0x4B, 0x03, 0x04, 0x14, 0x00, 0x00, 0x00, 0x08, 0x00];
    zipish.extend_from_slice(&garbage(64, 3));
    for ext in ["docx", "pptx", "xlsx", "odt", "odp", "epub"] {
        assert_no_panic(&zipish, &format!("truncated.{ext}"));
    }

    // A CFB signature with a truncated header — the .doc/.ppt/.msg containers.
    let mut cfbish = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    cfbish.extend_from_slice(&[0u8; 24]);
    for ext in ["doc", "ppt", "msg", "xls"] {
        assert_no_panic(&cfbish, &format!("truncated.{ext}"));
    }

    // A PDF header with a body that stops immediately.
    assert_no_panic(b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog", "truncated.pdf");
}
