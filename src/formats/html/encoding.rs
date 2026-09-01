//! Deciding what encoding an HTML document is in, and decoding it.
//!
//! HTML was the last format still assuming UTF-8. It read the same document
//! three different ways — `fs::read_to_string` at two entry points (which
//! **fails** on non-UTF-8 with a bare `io::Error`) and `String::from_utf8_lossy`
//! at four others (which silently replaces every non-UTF-8 byte with U+FFFD).
//! So whether a windows-1251 page errored, came back as mojibake, or worked at
//! all depended on which function you happened to call.
//!
//! Two files in the corpus proved it: `tika_big-preamble.html` declares
//! `windows-1251` and `tika_noisy-meta-encoding-arabic.html` declares
//! `iso-8859-6`. Both competitors read them; this engine returned an error.
//! They were 2 of only 3 legitimate documents in a 638-file corpus it could not
//! read (TECH_DEBT C7).
//!
//! ## Precedence, and where it deliberately differs from the WHATWG spec
//!
//! 1. **BOM** — wins outright, as the spec says.
//! 2. **Valid UTF-8** — wins next. The spec would consult `<meta charset>`
//!    first. We do not, on purpose: it makes every file that decodes today
//!    decode identically tomorrow, so this change cannot alter the output of a
//!    document that was already correct. A file that is valid UTF-8 while
//!    declaring something else is mislabelled, and trusting the bytes that
//!    actually decode is the safer reading. This mirrors the `"auto"` policy
//!    C4 introduced for CSV.
//! 3. **`<meta charset>`** — the document's own declaration, honoured through
//!    `encoding_rs`, which speaks every WHATWG label.
//! 4. **Detection** — `text_encoding::decode_text`, the same BOM/UTF-16/cp1252
//!    ladder the other text formats use.
//!
//! Steps 3 and 4 only ever run for bytes that are *not* valid UTF-8, which is
//! precisely the set that used to fail or mangle.

use encoding_rs::Encoding;

use crate::text_encoding::{decode_text, normalize_newlines};

/// How far into the document to look for a `<meta>` charset declaration.
///
/// The WHATWG pre-scan uses 1024 bytes and so do we. It has to be a byte scan:
/// you cannot decode the document until you know its encoding, and you cannot
/// know its encoding until you have read the declaration. That circularity is
/// resolved by the fact that every encoding worth supporting here is
/// ASCII-compatible in tag syntax, so the declaration is readable from the raw
/// bytes without decoding anything.
const META_SNIFF_BYTES: usize = 1024;

/// Decode HTML bytes to a string, honouring the document's declared encoding.
///
/// Never fails: an unrecognised or absent declaration falls through to
/// detection, and detection always produces something. Line endings are
/// normalised, for the same reason every other format normalises them — the
/// block splitters look for a literal `"\n\n"`.
pub(crate) fn decode_html(bytes: &[u8]) -> String {
    // 1. A BOM is unambiguous, and `decode_text` already strips and honours it.
    if starts_with_bom(bytes) {
        return decode_text(bytes).0;
    }

    // 2. BOM-less UTF-16, for the reason `text_encoding::decode_raw` spells out:
    //    ASCII in UTF-16LE is "T\0h\0e\0", and NUL is a valid UTF-8 codepoint,
    //    so `from_utf8` *accepts* it and returns the NUL-interleaved bytes. This
    //    check sat after the UTF-8 one and so never ran — measured, a BOM-less
    //    UTF-16 page came back with its tags unparsed, while the same bytes as
    //    `.txt` decoded correctly. Prose that is genuinely UTF-8 never has 20%
    //    NULs in one parity class, so nothing that decodes today can change.
    if crate::text_encoding::sniffs_utf16(bytes) {
        return decode_text(bytes).0;
    }

    // 3. Bytes that already decode as UTF-8 keep decoding as UTF-8, so nothing
    //    that worked before can change.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return normalize_newlines(text.to_string());
    }

    // 4. The document's own declaration.
    if let Some(encoding) = declared_encoding(bytes) {
        let (text, _, _) = encoding.decode(bytes);
        return normalize_newlines(text.into_owned());
    }

    // 5. Detection, shared with the other text formats.
    decode_text(bytes).0
}

fn starts_with_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xEF, 0xBB, 0xBF])
        || bytes.starts_with(&[0xFF, 0xFE])
        || bytes.starts_with(&[0xFE, 0xFF])
}

/// Find the encoding named by a `<meta>` tag in the document's first 1 KiB.
///
/// Handles both spellings the wild uses:
///   `<meta charset="windows-1251">`
///   `<meta http-equiv="Content-Type" content="text/html; charset=iso-8859-6">`
///
/// The search is confined to the inside of a `<meta …>` tag rather than run
/// over the whole prefix, so a `charset=` sitting in a URL or a script string
/// cannot be mistaken for a declaration.
fn declared_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = &bytes[..bytes.len().min(META_SNIFF_BYTES)];
    // Lossy is right here: any byte this mangles is outside a tag name or an
    // encoding label, both of which are ASCII.
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();

    let mut rest = text.as_str();
    while let Some(at) = rest.find("<meta") {
        rest = &rest[at + 5..];
        let tag = match rest.find('>') {
            Some(end) => &rest[..end],
            None => rest, // truncated by the 1 KiB window — still worth reading
        };
        if let Some(label) = charset_label(tag) {
            if let Some(encoding) = Encoding::for_label(label.as_bytes()) {
                return Some(encoding);
            }
        }
    }
    None
}

/// Pull the value out of a `charset=…` inside one tag's attributes.
fn charset_label(tag: &str) -> Option<String> {
    let at = tag.find("charset")?;
    let after = tag[at + 7..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let value = match after.strip_prefix(['"', '\'']) {
        // Quoted: run to the closing quote.
        Some(inner) => inner.split(['"', '\'']).next().unwrap_or(inner),
        // Bare: run to the first delimiter. The quote characters belong in this
        // set even though the value is unquoted, because of the `http-equiv`
        // long form: in
        //   <meta http-equiv="Content-Type" content="text/html; charset=cp1251">
        // the `charset=` value is not itself quoted — the quote that follows it
        // terminates the *enclosing* `content` attribute. Without them the label
        // came out as `cp1251"` and `Encoding::for_label` rejected it, so the
        // document silently fell back to windows-1252. An unquoted attribute
        // value cannot legally contain a quote in any case.
        None => after
            .split([' ', '\t', '\n', '\r', ';', '/', '>', '"', '\''])
            .next()
            .unwrap_or(after),
    };
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_untouched_even_when_mislabelled() {
        // The deliberate deviation from WHATWG: bytes that decode win over a
        // declaration that disagrees, so no currently-correct file can change.
        let src = b"<meta charset=\"windows-1251\"><p>caf\xc3\xa9</p>";
        assert!(decode_html(src).contains("café"));
    }

    #[test]
    fn declared_encoding_is_honoured_for_non_utf8_bytes() {
        // 0xEF is "п" in windows-1251 and invalid as a lone UTF-8 lead byte.
        let src = b"<meta charset=\"windows-1251\"><p>\xef\xf0\xe8</p>";
        let out = decode_html(src);
        assert!(out.contains("при"), "got {out:?}");
        assert!(!out.contains('\u{FFFD}'), "no replacement chars: {out:?}");
    }

    #[test]
    fn http_equiv_content_type_spelling_works() {
        let src = b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-6\"><p>\xc7\xe4</p>";
        let out = decode_html(src);
        assert!(!out.contains('\u{FFFD}'), "got {out:?}");
    }

    #[test]
    fn unquoted_charset_value_works() {
        let src = b"<meta charset=windows-1251><p>\xef</p>";
        assert!(!decode_html(src).contains('\u{FFFD}'));
    }

    #[test]
    fn charset_outside_a_meta_tag_is_ignored() {
        // A URL mentioning charset must not be read as a declaration; with no
        // real declaration these bytes fall through to detection (cp1252).
        let src = b"<a href=\"/x?charset=windows-1251\">l</a><p>\x93q\x94</p>";
        let out = decode_html(src);
        assert!(
            out.contains('\u{201C}') && out.contains('\u{201D}'),
            "got {out:?}"
        );
    }

    #[test]
    fn unknown_label_falls_through_to_detection() {
        let src = b"<meta charset=\"not-a-real-encoding\"><p>\x93q\x94</p>";
        let out = decode_html(src);
        assert!(!out.is_empty());
        assert!(out.contains('\u{201C}'), "cp1252 fallback: {out:?}");
    }

    #[test]
    fn declaration_past_the_sniff_window_is_not_read() {
        let mut src = b"<!--".to_vec();
        src.extend(std::iter::repeat_n(b' ', META_SNIFF_BYTES));
        src.extend_from_slice(b"--><meta charset=\"windows-1251\"><p>\xef</p>");
        // Falls through to detection rather than reading a far-away tag.
        assert!(!decode_html(&src).is_empty());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for src in [
            &b""[..],
            &b"<meta charset="[..],
            &b"<meta charset=\""[..],
            &b"<meta"[..],
            &[0xFF, 0xFE][..],
            &[0x00, 0x00, 0x00][..],
        ] {
            let _ = decode_html(src);
        }
    }
}
