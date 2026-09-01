//! Codepage/charset resolution for RTF, plus symbol-font recognition.

use encoding_rs::{
    Encoding, BIG5, EUC_KR, GBK, SHIFT_JIS, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252, WINDOWS_1253,
    WINDOWS_1254, WINDOWS_1255, WINDOWS_1256,
};

/// `\fcharset2` — SYMBOL_CHARSET. Bytes in such a font are glyph indices into a
/// pictorial font, not characters, so they must never reach a text decoder.
pub const SYMBOL_CHARSET: i32 = 2;

/// Map an RTF `\fcharsetN` value to an `encoding_rs` codepage encoding.
pub fn charset_to_encoding(charset: i32) -> &'static Encoding {
    match charset {
        128 => SHIFT_JIS,    // Japanese
        134 => GBK,          // Simplified Chinese
        136 => BIG5,         // Traditional Chinese
        129 => EUC_KR,       // Korean
        238 => WINDOWS_1250, // Eastern European
        204 => WINDOWS_1251, // Cyrillic
        161 => WINDOWS_1253, // Greek
        162 => WINDOWS_1254, // Turkish
        177 => WINDOWS_1255, // Hebrew
        178 => WINDOWS_1256, // Arabic
        _ => WINDOWS_1252,
    }
}

/// Map an `\ansicpgN` code page number to an encoding.
pub fn codepage_to_encoding(cp: u32) -> &'static Encoding {
    match cp {
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        _ => WINDOWS_1252,
    }
}

/// Find `\ansicpgN` near the header to set the default `\'xx` encoding.
pub fn detect_ansicpg(bytes: &[u8]) -> &'static Encoding {
    let scan = &bytes[..bytes.len().min(512)];
    if let Some(pos) = find(scan, b"\\ansicpg") {
        let mut k = pos + 8;
        let mut num = 0u32;
        while k < scan.len() && scan[k].is_ascii_digit() {
            num = num * 10 + (scan[k] - b'0') as u32;
            k += 1;
        }
        if num > 0 {
            return codepage_to_encoding(num);
        }
    }
    WINDOWS_1252
}

/// Fonts whose bytes are glyph indices rather than text.
///
/// `\fcharset2` is the spec signal, but it is not reliable on its own: LibreOffice
/// writes `OpenSymbol` as `\fcharset128` (Shift-JIS), so its bullet byte `0x96`
/// decodes to U+FFFD. Matching the family name as well is what stops those glyphs
/// reaching a text decoder at all.
pub fn is_symbol_font(charset: Option<i32>, name: &str) -> bool {
    if charset == Some(SYMBOL_CHARSET) {
        return true;
    }
    let n = name.trim().to_ascii_lowercase();
    const SYMBOL_FAMILIES: [&str; 8] = [
        "symbol",
        "wingdings",
        "webdings",
        "opensymbol",
        "starsymbol",
        "zapfdingbats",
        "monotype sorts",
        "marlett",
    ];
    SYMBOL_FAMILIES
        .iter()
        .any(|f| n == *f || n.starts_with(&format!("{f} ")))
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}
