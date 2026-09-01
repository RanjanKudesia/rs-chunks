//! Font table pre-scan: per-font encoding, and which fonts are pictorial.

use std::collections::HashMap;

use encoding_rs::Encoding;

use super::encoding::{charset_to_encoding, is_symbol_font};
use super::scan;

pub struct Font {
    pub encoding: &'static Encoding,
    /// Bytes in this font are glyph indices, not characters.
    pub symbol: bool,
}

pub type Fonts = HashMap<i32, Font>;

/// Parse `{\fonttbl{\f0…;}{\f1…;}}` into font number → font.
pub fn parse(bytes: &[u8], default_enc: &'static Encoding) -> Fonts {
    let mut out = Fonts::new();
    scan::for_each_entry(bytes, b"{\\fonttbl", |def| {
        let Some(num) = font_number(def) else { return };
        let charset = scan::read_param(def, b"\\fcharset").map(|c| c as i32);
        let name = scan::read_trailing_name(def, default_enc).unwrap_or_default();
        out.insert(
            num,
            Font {
                encoding: charset.map_or(default_enc, charset_to_encoding),
                symbol: is_symbol_font(charset, &name),
            },
        );
    });
    out
}

/// `\fN` opens a font definition and must be its first control word.
fn font_number(def: &[u8]) -> Option<i32> {
    let rest = def.strip_prefix(b"{\\f")?;
    let digits: Vec<u8> = rest
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(&digits).ok()?.parse().ok()
}
