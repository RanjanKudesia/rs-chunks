//! PDF fonts: character codes → text, plus the advance widths layout needs.
//!
//! A PDF string is a sequence of *codes*, not characters. Turning them into text
//! needs the font's encoding, and turning them into positions needs its widths.
//! Both live in the font dictionary, in four different shapes:
//!
//! - a named base encoding (`/WinAnsiEncoding`, …);
//! - a `/Differences` array remapping individual codes to glyph *names*;
//! - a `/ToUnicode` CMap, which overrides everything when present;
//! - `/DescendantFonts` for composite (Type0/CID) fonts, whose codes are 2 bytes.
//!
//! The corpus says all four matter: of 581 fonts in `test_files/pdf`, 283 use a
//! `/Differences` array and 215 of those have neither a base encoding nor a
//! `/ToUnicode` — so a glyph-name table is load-bearing, not a nicety.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

use super::cambria;
use super::cmap;
use super::encoding_tables::{
    CodeToUnicode, MAC_ROMAN_ENCODING, STANDARD_ENCODING, SYMBOL_ENCODING, WIN_ANSI_ENCODING,
};
use super::glyph_names::GLYPH_NAMES;
use super::type1;

/// Width used when a font declares none. Half an em is the usual fallback and
/// only affects spacing heuristics, never the characters themselves.
const FALLBACK_WIDTH: f32 = 0.5;

/// One decoded code: its text and its advance, in text-space units (1 = one em).
pub(crate) struct Decoded {
    pub text: String,
    pub width: f32,
    /// True for the single-byte code 32 — the only code that may be treated as a
    /// word gap by the layout pass (`Tw` applies to it and nothing else).
    pub is_space_code: bool,
}

pub(crate) struct Font {
    two_byte: bool,
    to_unicode: HashMap<u32, String>,
    simple: Box<CodeToChar>,
    widths: HashMap<u32, f32>,
    default_width: f32,
    pub bold: bool,
    pub italic: bool,
}

type CodeToChar = [Option<char>; 256];

impl Font {
    pub fn from_dict(doc: &Document, dict: &Dictionary) -> Font {
        let subtype = name_of(dict.get(b"Subtype").ok()).unwrap_or_default();
        let composite = subtype == "Type0";
        let descendant = if composite {
            descendant_font(doc, dict)
        } else {
            None
        };
        // A Type0 font's descriptor and widths live on the descendant; a simple
        // font's live on the font dictionary itself.
        let metrics_dict = descendant.unwrap_or(dict);

        let base_font = name_of(dict.get(b"BaseFont").ok()).unwrap_or_default();
        let descriptor = metrics_dict
            .get_deref(b"FontDescriptor", doc)
            .and_then(Object::as_dict)
            .ok();

        // The declared widths are read before the encoding because the encoding
        // may need them: a subset that names its glyphs by index is decodable
        // only if its own widths vouch for the reference table (`cambria`).
        let declared = if composite {
            HashMap::new()
        } else {
            simple_widths(doc, dict)
        };
        let simple = if composite {
            [None; 256]
        } else {
            simple_encoding(doc, dict, &base_font, descriptor, &declared)
        };

        Font {
            two_byte: composite,
            to_unicode: dict
                .get_deref(b"ToUnicode", doc)
                .and_then(Object::as_stream)
                .ok()
                .and_then(|s| s.decompressed_content().ok())
                .map(|bytes| cmap::parse_to_unicode(&bytes))
                .unwrap_or_default(),
            widths: if composite {
                cid_widths(doc, metrics_dict)
            } else if declared.is_empty() {
                // A base-14 font may omit /Widths entirely — the viewer is
                // expected to know the metrics. Without them every glyph
                // advanced FALLBACK_WIDTH, which drifts far enough to
                // interleave adjacent text runs (TECH_DEBT #92, #93).
                base14_widths(&base_font, &simple)
            } else {
                declared
            },
            simple: Box::new(simple),
            default_width: default_width(doc, composite, metrics_dict, descriptor),
            bold: is_bold(&base_font, descriptor),
            italic: is_italic(&base_font, descriptor),
        }
    }

    /// Split a PDF string into codes and decode each one.
    pub fn decode(&self, bytes: &[u8]) -> Vec<Decoded> {
        let mut out = Vec::with_capacity(bytes.len());
        if self.two_byte {
            for pair in bytes.chunks(2) {
                let code = match pair {
                    [hi, lo] => ((*hi as u32) << 8) | *lo as u32,
                    [only] => *only as u32,
                    _ => continue,
                };
                out.push(self.decode_code(code, false));
            }
        } else {
            for b in bytes {
                out.push(self.decode_code(*b as u32, *b == 32));
            }
        }
        out
    }

    fn decode_code(&self, code: u32, is_space_code: bool) -> Decoded {
        // /ToUnicode wins: it is the producer's own statement of what the code
        // means, and is the only correct source for subset fonts whose glyph
        // names are arbitrary (`g17`, `cid42`).
        let text = match self.to_unicode.get(&code) {
            Some(t) => t.clone(),
            None => self
                .simple
                .get(code as usize)
                .copied()
                .flatten()
                .map(String::from)
                .unwrap_or_default(),
        };
        Decoded {
            text: expand_ligatures(text),
            width: self
                .widths
                .get(&code)
                .copied()
                .unwrap_or(self.default_width),
            is_space_code,
        }
    }
}

/// Spell out the Latin typographic ligatures.
///
/// A PDF sets `classification` with one `ﬁ` glyph, and both the glyph name `fi`
/// and a `/ToUnicode` entry resolve it to U+FB01. Keeping that codepoint leaves
/// the word unsearchable — `classiﬁcation` does not match a query for
/// `classification` — so the presentation form is written out as the letters it
/// stands for. On `arxiv_1409.1556_vgg` alone that is 211 occurrences.
fn expand_ligatures(text: String) -> String {
    if !text.chars().any(|c| ('\u{FB00}'..='\u{FB06}').contains(&c)) {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            other => out.push(other),
        }
    }
    out
}

fn descendant_font<'a>(doc: &'a Document, dict: &'a Dictionary) -> Option<&'a Dictionary> {
    let array = dict
        .get_deref(b"DescendantFonts", doc)
        .and_then(Object::as_array)
        .ok()?;
    let first = array.first()?;
    doc.dereference(first)
        .ok()
        .and_then(|(_, o)| o.as_dict().ok())
}

/// Build a simple font's 256-entry code → character table: base encoding first,
/// then `/Differences` on top of it.
fn simple_encoding(
    doc: &Document,
    dict: &Dictionary,
    base_font: &str,
    descriptor: Option<&Dictionary>,
    declared: &HashMap<u32, f32>,
) -> CodeToChar {
    let encoding = dict.get_deref(b"Encoding", doc).ok();
    let named = match &encoding {
        Some(Object::Name(n)) => Some(String::from_utf8_lossy(n).to_string()),
        Some(Object::Dictionary(d)) => name_of(d.get(b"BaseEncoding").ok()),
        _ => None,
    };
    let named: Option<&CodeToUnicode> = match named.as_deref() {
        Some("WinAnsiEncoding") => Some(&WIN_ANSI_ENCODING),
        Some("MacRomanEncoding") => Some(&MAC_ROMAN_ENCODING),
        Some("StandardEncoding") => Some(&STANDARD_ENCODING),
        _ => None,
    };

    let mut table: CodeToChar = [None; 256];
    match named {
        Some(base) => copy(base, &mut table),
        // The font dictionary names no encoding, so the font program's own is
        // the authority. TeX's maths fonts are the case that matters: symbolic,
        // encoding-less, and placing `/alpha` at code 11.
        None => match builtin_program(doc, descriptor)
            .as_deref()
            .and_then(type1::builtin_encoding)
        {
            Some(builtin) => table = builtin,
            None => {
                let symbolic = descriptor
                    .and_then(|d| d.get(b"Flags").and_then(Object::as_i64).ok())
                    .is_some_and(|f| f & 0b100 != 0);
                if base_font.contains("Symbol") {
                    copy(&SYMBOL_ENCODING, &mut table);
                } else if !symbolic {
                    copy(&STANDARD_ENCODING, &mut table);
                }
                // A symbolic font with no readable encoding is left empty:
                // /Differences or /ToUnicode will speak for it, and guessing a
                // Latin table over a dingbat font would invent letters.
            }
        },
    }
    if let Some(Object::Dictionary(d)) = &encoding {
        let bare = apply_differences(doc, d, &mut table);
        // A subset that names glyphs `/g18` has said nothing about them. The
        // reference table can, but only if the font's widths vouch for it.
        if let Some(resolved) = cambria::resolve(base_font, &bare, declared) {
            for (code, ch) in resolved {
                table[code] = Some(ch);
            }
        }
    }
    table
}

fn copy(source: &CodeToUnicode, table: &mut CodeToChar) {
    for (i, slot) in source.iter().enumerate() {
        table[i] = slot.and_then(|u| char::from_u32(u as u32));
    }
}

/// The embedded Type 1 program, if the descriptor carries one. `/FontFile2`
/// (TrueType) and `/FontFile3` (CFF) keep their encodings in binary tables and
/// are not read here.
fn builtin_program(doc: &Document, descriptor: Option<&Dictionary>) -> Option<Vec<u8>> {
    let stream = descriptor?
        .get_deref(b"FontFile", doc)
        .and_then(Object::as_stream)
        .ok()?;
    stream.decompressed_content().ok()
}

/// `/Differences` is a flat array of `code name name … code name …`: each number
/// resets the code counter, each name assigns and advances it.
///
/// Returns the `(code, index)` pairs whose names were bare glyph indices that
/// resolved to nothing — the raw material [`cambria::resolve`] works from.
fn apply_differences(
    doc: &Document,
    encoding: &Dictionary,
    table: &mut CodeToChar,
) -> Vec<(usize, u16)> {
    let mut bare = Vec::new();
    let Ok(items) = encoding
        .get_deref(b"Differences", doc)
        .and_then(Object::as_array)
    else {
        return bare;
    };
    let mut code = 0usize;
    for item in items {
        match item {
            Object::Integer(n) => code = (*n).max(0) as usize,
            Object::Real(n) => code = (*n).max(0.0) as usize,
            Object::Name(n) => {
                let name = String::from_utf8_lossy(n);
                if code < 256 && !names_its_own_code(&name, code) {
                    table[code] = glyph_to_char(&name);
                    if table[code].is_none() {
                        if let Some(index) = bare_index(&name) {
                            bare.push((code, index));
                        }
                    }
                }
                code += 1;
            }
            _ => {}
        }
    }
    bare
}

/// A `/Differences` name that only restates the code it sits at — `/a65` at
/// code 65 — carries no information, so it must not overwrite the base encoding
/// with nothing.
///
/// dvips-produced Type 3 fonts name **every** glyph that way, which is why
/// `arxiv_2005.14165_gpt3.pdf`'s two bitmap fonts and `arxiv_1506.02640_yolo`'s
/// decoded to silence. Ignoring the name lets StandardEncoding speak for the
/// code, and the page comes out as English.
///
/// The self-reference *is* the test, and it is what makes this safe: a real
/// glyph called `a84` exists — it is a ZapfDingbats name, used by
/// `pdfjs_freeculture.pdf` — but it sits at code 116, so it is untouched. Across
/// the whole PDF corpus the only fonts where the index equals the code are the
/// four Type 3 ones. Leading zeros are rejected so `uni0041`-style names, which
/// do carry information, can never be read as self-referential.
fn names_its_own_code(name: &str, code: usize) -> bool {
    bare_index(name).is_some_and(|index| usize::from(index) == code)
}

/// `/g18` → 18. Only `g` and `a` are treated as index prefixes, and only
/// without a leading zero — those are the conventions the corpus actually uses.
fn bare_index(name: &str) -> Option<u16> {
    let digits = name.strip_prefix('g').or_else(|| name.strip_prefix('a'))?;
    if digits.starts_with('0') && digits.len() > 1 {
        return None;
    }
    digits.parse().ok()
}

/// Resolve a glyph name to a character.
pub(crate) fn glyph_to_char(name: &str) -> Option<char> {
    if name == ".notdef" {
        return None;
    }
    if let Ok(i) = GLYPH_NAMES.binary_search_by(|(n, _)| (*n).cmp(name)) {
        return char::from_u32(GLYPH_NAMES[i].1 as u32);
    }
    // `uniXXXX` / `uXXXX`…`uXXXXXX` are the spec's algorithmic forms.
    if let Some(hex) = name.strip_prefix("uni").filter(|h| h.len() >= 4) {
        if let Ok(v) = u32::from_str_radix(&hex[..4], 16) {
            return char::from_u32(v);
        }
    }
    if let Some(hex) = name
        .strip_prefix('u')
        .filter(|h| (4..=6).contains(&h.len()))
    {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            return char::from_u32(v);
        }
    }
    // A suffixed variant (`a.sc`, `one.oldstyle`) means the base glyph.
    if let Some((base, _)) = name.split_once('.') {
        if !base.is_empty() {
            return glyph_to_char(base);
        }
    }
    // A one-character name is that character (some subset fonts do this).
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Widths for a simple font that declares none, from the base-14 metrics.
///
/// Resolved through the font's own encoding array, so a `/Differences` that
/// remaps codes is honoured — the width follows the character actually drawn,
/// not the code point. Returns an empty map for a font that is not one of the
/// standard 14, which leaves `default_width` in charge exactly as before.
fn base14_widths(base_font: &str, encoding: &[Option<char>; 256]) -> HashMap<u32, f32> {
    let Some(metrics) = super::base14::widths_for(base_font) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (code, ch) in encoding.iter().enumerate() {
        if let Some(width) = ch.and_then(|c| metrics.width(c)) {
            out.insert(code as u32, width);
        }
    }
    out
}

fn simple_widths(doc: &Document, dict: &Dictionary) -> HashMap<u32, f32> {
    let mut out = HashMap::new();
    let first = dict
        .get_deref(b"FirstChar", doc)
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let Ok(widths) = dict.get_deref(b"Widths", doc).and_then(Object::as_array) else {
        return out;
    };
    for (i, w) in widths.iter().enumerate() {
        let Ok(w) = number(doc, w) else { continue };
        let code = first + i as i64;
        if (0..=255).contains(&code) {
            out.insert(code as u32, w / 1000.0);
        }
    }
    out
}

/// `/W` is `[ c [w w …]  cFirst cLast w  … ]` — a run of individual widths after
/// a start code, or one width shared by an inclusive code range.
fn cid_widths(doc: &Document, descendant: &Dictionary) -> HashMap<u32, f32> {
    let mut out = HashMap::new();
    let Ok(items) = descendant.get_deref(b"W", doc).and_then(Object::as_array) else {
        return out;
    };
    let mut i = 0;
    while i < items.len() {
        let Ok(first) = number(doc, &items[i]) else {
            break;
        };
        let Some(next) = items.get(i + 1) else { break };
        match doc.dereference(next).map(|(_, o)| o) {
            Ok(Object::Array(list)) => {
                for (k, w) in list.iter().enumerate() {
                    if let Ok(w) = number(doc, w) {
                        out.insert(first as u32 + k as u32, w / 1000.0);
                    }
                }
                i += 2;
            }
            _ => {
                let (Ok(last), Some(w)) = (number(doc, next), items.get(i + 2)) else {
                    break;
                };
                let Ok(w) = number(doc, w) else { break };
                // Guard the range: a malformed /W must not allocate unboundedly.
                let last = (last as u32).min(first as u32 + 65_535);
                for code in first as u32..=last {
                    out.insert(code, w / 1000.0);
                }
                i += 3;
            }
        }
    }
    out
}

fn default_width(
    doc: &Document,
    composite: bool,
    metrics: &Dictionary,
    descriptor: Option<&Dictionary>,
) -> f32 {
    if composite {
        // /DW defaults to 1000 for CID fonts (PDF 32000-1 §9.7.4.3).
        return metrics
            .get_deref(b"DW", doc)
            .ok()
            .and_then(|o| number(doc, o).ok())
            .unwrap_or(1000.0)
            / 1000.0;
    }
    descriptor
        .and_then(|d| d.get(b"MissingWidth").ok())
        .and_then(|o| number(doc, o).ok())
        .map(|w| w / 1000.0)
        .unwrap_or(FALLBACK_WIDTH)
}

fn is_bold(base_font: &str, descriptor: Option<&Dictionary>) -> bool {
    if base_font.to_ascii_lowercase().contains("bold") {
        return true;
    }
    match descriptor
        .and_then(|d| d.get(b"StemV").ok())
        .and_then(|o| o.as_float().ok())
    {
        // 120 is comfortably above a regular weight's stem and below a black's.
        Some(stem) => stem >= 120.0,
        None => descriptor
            .and_then(|d| d.get(b"Flags").and_then(Object::as_i64).ok())
            .is_some_and(|f| f & (1 << 18) != 0),
    }
}

fn is_italic(base_font: &str, descriptor: Option<&Dictionary>) -> bool {
    let lower = base_font.to_ascii_lowercase();
    if lower.contains("italic") || lower.contains("oblique") {
        return true;
    }
    if descriptor
        .and_then(|d| d.get(b"ItalicAngle").ok())
        .and_then(|o| o.as_float().ok())
        .is_some_and(|a| a != 0.0)
    {
        return true;
    }
    descriptor
        .and_then(|d| d.get(b"Flags").and_then(Object::as_i64).ok())
        .is_some_and(|f| f & (1 << 6) != 0)
}

fn number(doc: &Document, object: &Object) -> Result<f32, ()> {
    doc.dereference(object)
        .map(|(_, o)| o)
        .map_err(|_| ())?
        .as_float()
        .map_err(|_| ())
}

fn name_of(object: Option<&Object>) -> Option<String> {
    match object? {
        Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ligatures_are_spelled_out_so_the_word_stays_searchable() {
        assert_eq!(
            expand_ligatures("classi\u{FB01}cation".into()),
            "classification"
        );
        assert_eq!(expand_ligatures("su\u{FB03}x".into()), "suffix");
        assert_eq!(expand_ligatures("plain".into()), "plain");
    }

    #[test]
    fn algorithmic_glyph_names_resolve_and_notdef_does_not() {
        assert_eq!(glyph_to_char("uni0041"), Some('A'));
        assert_eq!(glyph_to_char("u00042"), Some('B'));
        assert_eq!(glyph_to_char("alpha"), Some('α'));
        // A TeX maths name plain AGL does not carry.
        assert_eq!(glyph_to_char("summationdisplay"), Some('\u{2211}'));
        assert_eq!(glyph_to_char(".notdef"), None);
        // A suffixed variant falls back to its base glyph.
        assert_eq!(glyph_to_char("one.oldstyle"), Some('1'));
    }
}
