//! XML/text primitives shared by every DOCX walker.

use quick_xml::name::QName;

/// True when `name` matches `expected`, ignoring any XML namespace prefix
/// (e.g. matches `w:p` against `b"p"` and a bare `p` against `b"p"`).
pub(super) fn qname_eq(name: QName<'_>, expected: &[u8]) -> bool {
    let n = name.as_ref();
    n == expected || n.rsplit(|b| *b == b':').next() == Some(expected)
}

/// Append `piece` to `target`, trimming whitespace and inserting a single
/// space separator when both sides are non-empty. Used to stitch the run
/// fragments inside a paragraph back into a single string.
pub(super) fn push_text(target: &mut String, piece: &str) {
    // Spaces and line ends trim; TABS DO NOT. A run-content `<w:tab/>` reaches
    // this function as a piece containing '\t' (often the whole piece, because
    // producers put the tab in its own run), and `str::trim` was deleting it —
    // 283 tabs in one fixture became zero output characters, fusing
    // `column1<tab>Mid` into one word. Paragraph and sub-segment flushes trim
    // full whitespace at the EDGES, so a kept tab can never start an output
    // line (the GFM indented-code hazard).
    let trimmed = piece.trim_matches([' ', '\n', '\r'].as_slice());
    if trimmed.is_empty() {
        return;
    }
    // The joining space exists to separate whole-element pieces; a tab at the
    // boundary IS the separator, so adding a space beside it would inject
    // phantom whitespace into tab-delimited content.
    if !target.is_empty() && !target.ends_with('\t') && !trimmed.starts_with('\t') {
        target.push(' ');
    }
    target.push_str(trimmed);
}

/// Append text that came from an entity reference.
///
/// [`push_text`] space-joins successive pieces, which is right when each piece
/// is a whole element's text. An entity reference splits one element into
/// several events, so joining there inserts a space in the middle of a word:
/// `AT&amp;T` becomes `AT & T`. Entity text is appended exactly as-is instead.
// Currently unreferenced: entity events are handled inline by the extractors,
// but this documents the AT&T-splitting gotcha and is kept as the reference
// implementation ported from the reference engine.
#[allow(dead_code)]
pub(super) fn push_event_text(target: &mut String, piece: &str, is_entity: bool) {
    if is_entity {
        target.push_str(piece);
    } else {
        push_text(target, piece);
    }
}

/// Collapse runs of whitespace (including newlines/tabs) into single spaces
/// and trim the result. Mirrors the previous per-file implementations.
pub(super) fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }

    out.trim().to_string()
}
