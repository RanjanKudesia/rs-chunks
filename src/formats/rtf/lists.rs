//! `\listtext` → markdown list marker.
//!
//! `\listtext` holds the literal marker a writer painted for a list item: `1.` for
//! an ordered list, or a glyph from a pictorial font for a bullet. That glyph is
//! only meaningful in its own font — `0x96` in OpenSymbol, `U+F0FC` in Wingdings —
//! so carrying it into text yields U+FFFD or raw Private Use codepoints. Markdown
//! has exactly one bullet syntax, so the glyph is discarded and the *kind* of
//! marker is what survives.

/// Convert captured `\listtext` content into a markdown list marker.
pub fn marker_for(listtext: &str) -> String {
    let t = listtext.trim_matches(|c: char| c.is_whitespace() || c == '\u{00A0}');
    match ordered_marker(t) {
        Some(m) => format!("{m} "),
        None => "- ".to_string(),
    }
}

/// Recognise an ordered marker: `1.` `12)` `a.` `(b)` — the writer's own numbering
/// is kept, since a list need not start at 1.
fn ordered_marker(t: &str) -> Option<&str> {
    let mut core = t;
    core = core.strip_suffix(['.', ')']).unwrap_or(core);
    core = core.strip_prefix('(').unwrap_or(core);
    if core.is_empty() || core.len() > 12 {
        return None;
    }
    let numeric = core.chars().all(|c| c.is_ascii_digit());
    let alpha = core.len() <= 2 && core.chars().all(|c| c.is_ascii_alphabetic());
    if numeric || alpha {
        Some(t)
    } else {
        None
    }
}
