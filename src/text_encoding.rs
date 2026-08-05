//! Turning a plain-text file's bytes into a `String`, correctly.
//!
//! The txt reader used to do this:
//!
//! ```ignore
//! std::str::from_utf8(bytes).map(str::to_string)
//!     .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string())
//! ```
//!
//! which is wrong for two very ordinary files and silent about both:
//!
//! * **UTF-16** — what Windows Notepad writes when you pick "Unicode". Every
//!   ASCII character is a byte plus a NUL, so the lossy path yields interleaved
//!   replacement characters and the caller gets garbage with no error.
//! * **cp1252 / Latin-1** — every non-ASCII byte becomes U+FFFD, so smart
//!   quotes, em dashes, é and £ are all destroyed.
//!
//! A byte-order mark is also text as far as `from_utf8` is concerned, so a
//! UTF-8 BOM leaked into the first chunk (and into the first heading, where it
//! broke heading detection).
//!
//! Detection order: BOM, then a UTF-16 sniff, then valid UTF-8, then cp1252.
//! cp1252 is the fallback rather than Latin-1 because it is what "ANSI" means
//! in practice on the files this library actually sees, and it agrees with
//! Latin-1 everywhere except 0x80–0x9F — where Latin-1 has unused control codes
//! and cp1252 has the punctuation people actually type.

use encoding_rs::{UTF_16BE, UTF_16LE, WINDOWS_1252};

/// How many leading bytes the UTF-16 sniff looks at.
const SNIFF_BYTES: usize = 4096;
/// Fraction of NULs in one parity class needed to call a file UTF-16.
const NUL_RATIO: f32 = 0.20;

/// The encoding a byte slice was decoded as. Recorded so callers can report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl DetectedEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            DetectedEncoding::Utf8 => "utf-8",
            DetectedEncoding::Utf8Bom => "utf-8-bom",
            DetectedEncoding::Utf16Le => "utf-16le",
            DetectedEncoding::Utf16Be => "utf-16be",
            DetectedEncoding::Windows1252 => "windows-1252",
        }
    }
}

/// Decode plain-text bytes, detecting the encoding and stripping any BOM.
pub fn decode_text(bytes: &[u8]) -> (String, DetectedEncoding) {
    let (text, encoding) = decode_raw(bytes);
    (normalize_newlines(text), encoding)
}

/// Turn every line terminator into `\n`.
///
/// A document's paragraph break is two line terminators, and every block
/// splitter in the engine looks for the literal `"\n\n"`. A Windows file's
/// break is `"\r\n\r\n"`, which contains no `"\n\n"` — so a CRLF document
/// used to come back as a single unsplit chunk with no heading, no section and
/// no size bound, on every mode (TECH_DEBT #89). A classic Mac file, terminated
/// with bare `\r`, had no line breaks at all as far as `str::lines` was
/// concerned, and the raw CR reached chunk content (#90).
///
/// Normalising once here is the fix rather than teaching seven `split("\n\n")`
/// sites about `\r`: any splitter added later inherits it.
pub fn normalize_newlines(text: String) -> String {
    if !text.contains('\r') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            // CRLF is one terminator, not two.
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Decode a UTF-8 document, falling back to lossy, with line endings
/// normalised.
///
/// Markdown files are UTF-8 by contract, so this does not sniff encodings the
/// way [`decode_text`] does — but it must normalise newlines for the same
/// reason (TECH_DEBT #89): a CRLF `.md` file has no `"\n\n"` in it, so the
/// block parser saw one block and returned the whole document as one chunk.
///
/// Six copies of the decode expression were spread across `md/`'s strategy
/// files, which is why fixing this in one of them would have fixed one mode.
pub fn decode_utf8_document(bytes: &[u8]) -> String {
    let text = match std::str::from_utf8(bytes) {
        Ok(v) => v.to_string(),
        Err(_) => String::from_utf8_lossy(bytes).to_string(),
    };
    normalize_newlines(text)
}

/// Decode plain-text bytes without touching line endings.
fn decode_raw(bytes: &[u8]) -> (String, DetectedEncoding) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (lossy_utf8(rest), DetectedEncoding::Utf8Bom);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (UTF_16LE.decode(rest).0.into_owned(), DetectedEncoding::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (UTF_16BE.decode(rest).0.into_owned(), DetectedEncoding::Utf16Be);
    }
    // The UTF-16 sniff has to come BEFORE the UTF-8 check, not after: ASCII
    // encoded as UTF-16LE is "T\0h\0e\0", and NUL is a perfectly valid UTF-8
    // codepoint, so from_utf8 accepts it and hands back the NUL-interleaved
    // garbage rather than failing.
    if let Some(enc) = sniff_utf16(bytes) {
        let decoded = match enc {
            DetectedEncoding::Utf16Be => UTF_16BE.decode(bytes).0.into_owned(),
            _ => UTF_16LE.decode(bytes).0.into_owned(),
        };
        return (decoded, enc);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return (text.to_string(), DetectedEncoding::Utf8);
    }
    // Not UTF-8 and not UTF-16: an 8-bit encoding. cp1252 maps every byte to
    // something, so this never produces U+FFFD — it may be the wrong character
    // for a cp1251 or Big5 file, but it is never a destroyed one.
    (
        WINDOWS_1252.decode(bytes).0.into_owned(),
        DetectedEncoding::Windows1252,
    )
}

fn lossy_utf8(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Detect BOM-less UTF-16 from the NUL pattern.
///
/// Text that is mostly ASCII encodes as alternating value/NUL pairs, so the
/// NULs cluster in one parity class: odd offsets for little-endian, even for
/// big-endian. Requiring the *other* class to be nearly NUL-free is what keeps
/// a binary blob from being mistaken for UTF-16.
fn sniff_utf16(bytes: &[u8]) -> Option<DetectedEncoding> {
    let window = &bytes[..bytes.len().min(SNIFF_BYTES)];
    if window.len() < 4 {
        return None;
    }
    let (mut even_nuls, mut odd_nuls) = (0usize, 0usize);
    for (i, b) in window.iter().enumerate() {
        if *b == 0 {
            if i % 2 == 0 {
                even_nuls += 1;
            } else {
                odd_nuls += 1;
            }
        }
    }
    let half = (window.len() / 2) as f32;
    if half == 0.0 {
        return None;
    }
    let (even_ratio, odd_ratio) = (even_nuls as f32 / half, odd_nuls as f32 / half);
    if odd_ratio >= NUL_RATIO && even_ratio < NUL_RATIO / 4.0 {
        return Some(DetectedEncoding::Utf16Le);
    }
    if even_ratio >= NUL_RATIO && odd_ratio < NUL_RATIO / 4.0 {
        return Some(DetectedEncoding::Utf16Be);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str, bom: bool) -> Vec<u8> {
        let mut out = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    fn utf16be(s: &str, bom: bool) -> Vec<u8> {
        let mut out = if bom { vec![0xFE, 0xFF] } else { Vec::new() };
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_be_bytes());
        }
        out
    }

    const SAMPLE: &str = "The quick brown fox jumps over the lazy dog. Sentence two here.";

    #[test]
    fn plain_utf8_is_unchanged() {
        let (text, enc) = decode_text("héllo — wörld".as_bytes());
        assert_eq!(text, "héllo — wörld");
        assert_eq!(enc, DetectedEncoding::Utf8);
    }

    #[test]
    fn utf8_bom_is_stripped_not_leaked_into_the_text() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"# Heading");
        let (text, enc) = decode_text(&bytes);
        assert_eq!(text, "# Heading");
        assert!(!text.starts_with('\u{feff}'));
        assert_eq!(enc, DetectedEncoding::Utf8Bom);
    }

    #[test]
    fn utf16_with_bom_decodes_both_endiannesses() {
        assert_eq!(decode_text(&utf16le(SAMPLE, true)).0, SAMPLE);
        assert_eq!(decode_text(&utf16be(SAMPLE, true)).0, SAMPLE);
    }

    #[test]
    fn utf16_without_bom_is_sniffed_from_the_nul_pattern() {
        let (le, le_enc) = decode_text(&utf16le(SAMPLE, false));
        assert_eq!(le, SAMPLE);
        assert_eq!(le_enc, DetectedEncoding::Utf16Le);
        let (be, be_enc) = decode_text(&utf16be(SAMPLE, false));
        assert_eq!(be, SAMPLE);
        assert_eq!(be_enc, DetectedEncoding::Utf16Be);
    }

    #[test]
    fn cp1252_punctuation_survives_instead_of_becoming_replacement_chars() {
        // 0x93/0x94 smart quotes, 0x97 em dash, 0xE9 e-acute, 0xA3 pound.
        let bytes = [
            b'S', b'a', b'y', b' ', 0x93, b'h', b'i', 0x94, b' ', 0x97, b' ', 0xE9, b' ', 0xA3, b'5',
        ];
        let (text, enc) = decode_text(&bytes);
        assert_eq!(text, "Say “hi” — é £5");
        assert!(!text.contains('\u{fffd}'));
        assert_eq!(enc, DetectedEncoding::Windows1252);
    }

    #[test]
    fn a_nul_heavy_binary_blob_is_not_mistaken_for_utf16() {
        // NULs in both parity classes — the sniff must decline.
        let bytes: Vec<u8> = (0..512u16).map(|i| if i % 3 == 0 { 0 } else { 0xC3 }).collect();
        assert!(sniff_utf16(&bytes).is_none());
    }

    #[test]
    fn empty_and_tiny_inputs_do_not_panic() {
        assert_eq!(decode_text(b"").0, "");
        assert_eq!(decode_text(b"a").0, "a");
        assert_eq!(decode_text(&[0xFF, 0xFE]).0, "");
    }
}

#[cfg(test)]
mod newline_tests {
    use super::*;

    /// TECH_DEBT #89. Every block splitter in the engine looks for `"\n\n"`.
    /// A Windows paragraph break is `"\r\n\r\n"`, which does not contain it, so
    /// a CRLF document came back as one unsplit chunk on every mode.
    #[test]
    fn a_windows_paragraph_break_becomes_a_plain_one() {
        let (text, _) = decode_text(b"HEADING\r\n\r\nBody text.\r\n");
        assert_eq!(text, "HEADING\n\nBody text.\n");
        assert!(text.contains("\n\n"), "the block splitter must see a break");
    }

    /// TECH_DEBT #90. `str::lines()` strips a `\r` that precedes a `\n` but
    /// treats a lone `\r` as ordinary text, so a classic Mac document had no
    /// line breaks at all and the raw CR reached chunk content.
    #[test]
    fn a_bare_cr_becomes_a_line_feed() {
        let (text, _) = decode_text(b"FIRST\r\rSecond line.\r");
        assert_eq!(text, "FIRST\n\nSecond line.\n");
        assert!(!text.contains('\r'), "no raw CR may reach the caller");
    }

    #[test]
    fn mixed_terminators_all_normalise() {
        let (text, _) = decode_text(b"a\r\nb\rc\nd");
        assert_eq!(text, "a\nb\nc\nd");
    }

    /// CRLF is one terminator. Treating it as two would double every line
    /// break and turn every Windows line into its own block.
    #[test]
    fn crlf_is_one_terminator_not_two() {
        let (text, _) = decode_text(b"one\r\ntwo\r\nthree");
        assert_eq!(text.matches('\n').count(), 2);
    }

    /// UTF-16 is where CRLF is most likely — it is what Notepad writes.
    #[test]
    fn normalisation_applies_after_utf16_decoding() {
        let utf16: Vec<u8> = "HEAD\r\n\r\nBody.\r\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(utf16);
        let (text, enc) = decode_text(&bytes);
        assert_eq!(enc, DetectedEncoding::Utf16Le);
        assert_eq!(text, "HEAD\n\nBody.\n");
    }

    /// A document with no CR at all must come back untouched, and must not pay
    /// for a rebuild of the string.
    #[test]
    fn a_unix_document_is_returned_unchanged() {
        let src = "already\nnormal\n\ntext\n".to_string();
        assert_eq!(normalize_newlines(src.clone()), src);
    }
}
