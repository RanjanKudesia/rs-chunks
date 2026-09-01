//! Stylesheet pre-scan: which paragraph styles are headings, and at what level.
//!
//! RTF carries no inline heading markup — a heading is a paragraph whose `\sN`
//! names a heading *style*, defined once in `{\stylesheet …}`. The body tokenizer
//! skips that destination, so it is parsed here into `\sN → level` and consulted
//! when a paragraph selects a style.

use std::collections::HashMap;

use encoding_rs::Encoding;

use super::scan;

/// Heading levels are clamped to markdown's six.
const MAX_LEVEL: u8 = 6;

/// Style number → heading level (1-based).
pub type HeadingStyles = HashMap<i32, u8>;

/// Parse `{\stylesheet …}` and return the paragraph styles that are headings.
pub fn parse(bytes: &[u8], enc: &'static Encoding) -> HeadingStyles {
    let mut out = HeadingStyles::new();
    scan::for_each_entry(bytes, b"{\\stylesheet", |def| {
        if let Some((num, level)) = parse_style(def, enc) {
            out.insert(num, level);
        }
    });
    out
}

/// Parse one `{\sN … Style Name;}` definition.
///
/// Character styles (`{\*\csN …}`) and section styles (`{\dsN …}`) are not
/// paragraph styles and are skipped — `Heading 1 Char` is a run style, not a
/// heading, and treating it as one would mark inline spans as headings.
fn parse_style(def: &[u8], enc: &'static Encoding) -> Option<(i32, u8)> {
    let rest = def.strip_prefix(b"{\\s")?;
    let digits: Vec<u8> = rest
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let num: i32 = std::str::from_utf8(&digits).ok()?.parse().ok()?;
    // `\outlinelevelN` is the language-independent signal Word writes into every
    // heading style; the name match below is the fallback for writers that omit it.
    if let Some(level) = outline_level(def) {
        return Some((num, level));
    }
    let name = scan::read_trailing_name(def, enc)?;
    heading_level_from_name(&name).map(|level| (num, level))
}

fn outline_level(def: &[u8]) -> Option<u8> {
    let n = scan::read_param(def, b"\\outlinelevel")?;
    // \outlinelevel9 is "body text" — explicitly not a heading.
    if n >= 9 {
        return None;
    }
    Some(((n + 1) as u8).min(MAX_LEVEL))
}

/// Recognise a heading style by name, in the locales real writers emit.
///
/// Word and LibreOffice localise style names, so matching `heading N` alone would
/// miss most non-English documents; these are the names they actually write. A
/// name with no trailing digit (LibreOffice's generic `Heading`) is level 1.
fn heading_level_from_name(name: &str) -> Option<u8> {
    let lower = name.trim().to_lowercase();
    const HEADING_WORDS: [&str; 14] = [
        "heading",
        "überschrift",
        "uberschrift",
        "titre",
        "título",
        "titulo",
        "titolo",
        "kop",
        "overskrift",
        "rubrik",
        "otsikko",
        "заголовок",
        "標題",
        "見出し",
    ];
    let word = HEADING_WORDS.iter().find(|w| lower.starts_with(*w))?;
    let rest = lower[word.len()..]
        .trim_start_matches([' ', '-', '_'])
        .trim();
    if rest.is_empty() {
        return Some(1);
    }
    let level: u32 = rest.parse().ok()?;
    if level == 0 {
        return None;
    }
    Some((level as u8).min(MAX_LEVEL))
}
