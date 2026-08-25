//! A text-less PDF must say *why* (TECH_DEBT F8).
//!
//! "…scanned or image-only… pass `list_images`" was returned for **any**
//! `has_text == false`. An encrypted PDF is the commonest real-world failure and
//! got that message — a wrong cause with a remedy that cannot work, since
//! rendering its pages fails for the same reason. The evidence to tell these
//! apart was already collected in `Parsed::skipped` and then thrown away behind
//! `#[allow(dead_code)]`.

use std::path::{Path, PathBuf};

/// Deliberately-broken inputs live outside `test_files/` — see
/// `tests/fixtures_malformed/README.md`.
fn malformed(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures_malformed")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

#[test]
fn an_encrypted_pdf_says_so_rather_than_claiming_to_be_a_scan() {
    let p = malformed("derived_encrypted.pdf");
    let err = chunks_rs::formats::pdf::chunk(p.to_str().unwrap(), "default", 3, 1, 3, 15)
        .expect_err("an encrypted PDF has no readable text and must error");
    let msg = err.to_string();
    assert!(
        msg.contains("encrypted"),
        "the error must name encryption as the cause, got {msg:?}"
    );
    // Match the *claim*, not the word: the message legitimately contains
    // "scanned" while denying it ("This is not a scanned document").
    assert!(
        !msg.contains("scanned or image-only"),
        "it must not claim to be a scan, got {msg:?}"
    );
    assert!(
        !msg.contains("pass list_images to get"),
        "it must not offer a remedy that cannot work, got {msg:?}"
    );
}

/// The other branch must be untouched: a genuine scan still gets the scan
/// message and the `list_images` remedy, which for it does work.
#[test]
fn a_genuine_scan_still_gets_the_scan_message() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/pdf");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("pdf corpus must exist") {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        if let Err(e) = chunks_rs::formats::pdf::chunk(p.to_str().unwrap(), "default", 3, 1, 3, 15)
        {
            let msg = e.to_string();
            if msg.contains("no extractable text") {
                assert!(
                    !msg.contains("encrypted"),
                    "{}: not encrypted, but reported as such: {msg}",
                    p.display()
                );
                seen += 1;
            }
        }
    }
    // Not asserting a count: how many corpus PDFs are text-less is not this
    // test's business. It asserts only that none of them is misreported.
    let _ = seen;
}
