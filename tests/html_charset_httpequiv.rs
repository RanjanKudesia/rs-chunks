//! The `http-equiv` long form must resolve its charset.
//!
//! `<meta http-equiv="Content-Type" content="text/html; charset=X">` is the
//! pre-HTML5 spelling and still overwhelmingly common in legacy corpora — 11 of
//! 30 `.htm` fixtures in this workspace use it. In that form the `charset=`
//! value is NOT itself quoted: the quote that follows it closes the enclosing
//! `content` attribute. `charset_label`'s bare branch split on
//! `[' ', '\t', '\n', '\r', ';', '/', '>']` and not on the quote characters, so
//! the label came out as `windows-1251"`, `Encoding::for_label` rejected it, and
//! the document fell back to windows-1252 — mojibake, silently.
//!
//! This is deliberately asserted on RECOVERED TEXT rather than on the absence of
//! U+FFFD. `tests/html_encoding.rs` was found to assert
//! `md.matches('\u{FFFD}').count() == 0`, which a cp1252 fallback can essentially
//! never trip: cp1252 maps 251 of 256 bytes to real characters, so the failure
//! mode is mojibake, not replacement characters. That test passed while both
//! non-UTF-8 fixtures in the corpus were fully corrupted.

use chunks_rs::formats::html;

/// windows-1251 Cyrillic, declared in the long form.
fn cp1251_doc() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(
        b"<html><head><meta http-equiv=\"Content-Type\" \
          content=\"text/html; charset=windows-1251\"></head><body><p>",
    );
    // "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}" (privet) in windows-1251.
    v.extend_from_slice(&[0xEF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
    v.extend_from_slice(b"</p></body></html>");
    v
}

#[test]
fn the_http_equiv_long_form_resolves_its_charset() {
    let md = html::to_markdown_from_bytes(&cp1251_doc()).expect("must parse");
    assert!(
        md.contains("привет"),
        "declared windows-1251 was ignored; text came back mojibake: {md:?}"
    );
}

/// The short form already worked. Control: it must keep working.
#[test]
fn the_short_meta_charset_form_still_resolves() {
    let mut v = Vec::new();
    v.extend_from_slice(b"<html><head><meta charset=\"windows-1251\"></head><body><p>");
    v.extend_from_slice(&[0xEF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
    v.extend_from_slice(b"</p></body></html>");
    let md = html::to_markdown_from_bytes(&v).expect("must parse");
    assert!(md.contains("привет"), "short form regressed: {md:?}");
}

/// A UTF-8 document must not be disturbed by the widened delimiter set.
#[test]
fn a_utf8_document_is_unaffected() {
    let doc = "<html><head><meta charset=\"utf-8\"></head><body><p>héllo wörld</p></body></html>";
    let md = html::to_markdown_from_bytes(doc.as_bytes()).expect("must parse");
    assert!(md.contains("héllo wörld"), "utf-8 path disturbed: {md:?}");
}

/// The declaration-beyond-the-prescan case, on the real corpus file.
///
/// `tika_big-preamble.html` declares windows-1251 at byte ~4970, past the
/// WHATWG prescan window of 1024 bytes. Raising that window would diverge from
/// the spec, which is deliberately NOT what was done: the algorithm's own
/// fallback ladder ends in autodetection, and this engine had no detector — it
/// blanket-decoded as cp1252 and called that "detection". Measured before the
/// fix: 0 Cyrillic characters and 927 mojibake runs.
#[test]
fn a_declaration_past_the_prescan_window_is_still_recovered() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_files/html/tika_big-preamble.html");
    assert!(p.is_file(), "required fixture missing: {}", p.display());

    let bytes = std::fs::read(&p).expect("read fixture");
    let md = html::to_markdown_from_bytes(&bytes).expect("must parse");

    let cyrillic = md
        .chars()
        .filter(|c| ('\u{0410}'..='\u{044f}').contains(c))
        .count();
    assert!(
        cyrillic > 100,
        "expected recovered Cyrillic, got {cyrillic} chars: {:?}",
        md.chars().take(80).collect::<String>()
    );
}
