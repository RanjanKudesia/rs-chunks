//! Resolving XML/HTML entity references to the text they stand for.
//!
//! quick-xml 0.38 does not fold entity references into `Event::Text`. It emits
//! them as a separate `Event::GeneralRef` carrying just the name — `amp`,
//! `#8212`, `#x2014`. A walker that matches only on `Event::Text` therefore
//! *drops* them: `AT&amp;T` came out of the docx walker as `AT T`, with the
//! ampersand deleted rather than merely left encoded. Every XML walker in this
//! crate had that hole.
//!
//! Anything unrecognised is returned in its original `&name;` form. Losing an
//! exotic entity's meaning is bad; losing the characters entirely is worse, and
//! leaving `&name;` visible makes the gap obvious instead of silent.

/// Named entities worth resolving. The five XML predefined ones are mandatory;
/// the rest are the HTML named entities that actually turn up in office
/// documents and web pages — typography, currency, and legal symbols.
///
/// This is deliberately not the full HTML5 table (2000+ names, mostly
/// mathematical). Numeric references cover everything else, and they are how
/// Word, LibreOffice and Excel write non-ASCII in practice.
const NAMED: &[(&str, &str)] = &[
    // XML predefined — required by the spec.
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    // Spaces and dashes.
    ("nbsp", "\u{a0}"),
    ("ensp", "\u{2002}"),
    ("emsp", "\u{2003}"),
    ("thinsp", "\u{2009}"),
    ("shy", "\u{ad}"),
    ("ndash", "–"),
    ("mdash", "—"),
    ("minus", "−"),
    // Quotation marks.
    ("lsquo", "\u{2018}"),
    ("rsquo", "\u{2019}"),
    ("sbquo", "\u{201a}"),
    ("ldquo", "\u{201c}"),
    ("rdquo", "\u{201d}"),
    ("bdquo", "\u{201e}"),
    ("laquo", "«"),
    ("raquo", "»"),
    ("lsaquo", "\u{2039}"),
    ("rsaquo", "\u{203a}"),
    // Legal and commercial.
    ("copy", "©"),
    ("reg", "®"),
    ("trade", "™"),
    ("cent", "¢"),
    ("pound", "£"),
    ("yen", "¥"),
    ("euro", "€"),
    ("curren", "¤"),
    ("sect", "§"),
    ("para", "¶"),
    // Punctuation and symbols.
    ("hellip", "…"),
    ("bull", "•"),
    ("middot", "·"),
    ("dagger", "†"),
    ("Dagger", "‡"),
    ("prime", "′"),
    ("Prime", "″"),
    ("deg", "°"),
    ("plusmn", "±"),
    ("times", "×"),
    ("divide", "÷"),
    ("frac12", "½"),
    ("frac14", "¼"),
    ("frac34", "¾"),
    ("sup1", "¹"),
    ("sup2", "²"),
    ("sup3", "³"),
    ("micro", "µ"),
    ("permil", "‰"),
    ("infin", "∞"),
    ("ne", "≠"),
    ("le", "≤"),
    ("ge", "≥"),
    ("larr", "←"),
    ("rarr", "→"),
    ("harr", "↔"),
    ("iexcl", "¡"),
    ("iquest", "¿"),
    ("brvbar", "¦"),
    ("uml", "¨"),
    ("macr", "¯"),
    ("acute", "´"),
    ("cedil", "¸"),
    ("ordf", "ª"),
    ("ordm", "º"),
    ("not", "¬"),
];

/// Resolve one entity reference body — the text between `&` and `;`.
///
/// Handles `&#NN;` and `&#xHH;` numeric references and the named entities in
/// [`NAMED`]. Returns `&name;` verbatim for anything else, so unknown entities
/// stay visible in the output rather than vanishing from it.
pub fn resolve_entity(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('#') {
        let cp = match rest.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => rest.parse::<u32>().ok(),
        };
        if let Some(c) = cp.and_then(char::from_u32) {
            return c.to_string();
        }
        return format!("&{name};");
    }
    match NAMED.iter().find(|(n, _)| *n == name) {
        Some((_, text)) => (*text).to_string(),
        None => format!("&{name};"),
    }
}

/// Resolve a `GeneralRef` event's payload, given the raw entity name bytes.
pub fn resolve_entity_bytes(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(name) => resolve_entity(name),
        // Not UTF-8: emit nothing rather than mojibake, but this cannot happen
        // for a well-formed reference (entity names are ASCII by definition).
        Err(_) => String::new(),
    }
}

/// Read one event, folding an entity reference back into the text stream.
///
/// Every walker in this crate already knows what to do with `Event::Text`, and
/// an entity reference *is* text as far as extraction is concerned. Rather than
/// teach eighteen reader loops about `Event::GeneralRef` individually, this
/// turns the reference into the text it stands for and hands back a normal
/// `Event::Text` — so the existing handling applies unchanged.
///
/// `$spill` must be a `String` that lives at least as long as the returned
/// event borrows it; declaring it inside the loop body, immediately above the
/// call, is the simple correct choice.
///
/// `$is_entity` is set to true when the event came from a reference. That
/// matters: a reference splits one element's text into several events, and the
/// walkers that space-join successive events (correct when each event *was* a
/// whole element) would otherwise turn `AT&amp;T` into `AT & T`. Callers that
/// space-join must append entity text verbatim instead.
macro_rules! read_event_folding_entities {
    ($reader:expr, $buf:expr, $spill:expr, $is_entity:expr) => {
        match $reader.read_event_into($buf) {
            Ok(quick_xml::events::Event::GeneralRef(r)) => {
                *$spill = $crate::entities::resolve_entity_bytes(r.as_ref());
                *$is_entity = true;
                // from_escaped, not new: the resolved text is already literal,
                // and `new` would re-escape "&" straight back to "&amp;".
                Ok(quick_xml::events::Event::Text(
                    quick_xml::events::BytesText::from_escaped($spill.as_str()),
                ))
            }
            other => {
                *$is_entity = false;
                other
            }
        }
    };
    ($reader:expr, $buf:expr, $spill:expr) => {{
        let mut ignored = false;
        read_event_folding_entities!($reader, $buf, $spill, &mut ignored)
    }};
}

pub(crate) use read_event_folding_entities;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_five_xml_predefined_entities() {
        assert_eq!(resolve_entity("amp"), "&");
        assert_eq!(resolve_entity("lt"), "<");
        assert_eq!(resolve_entity("gt"), ">");
        assert_eq!(resolve_entity("quot"), "\"");
        assert_eq!(resolve_entity("apos"), "'");
    }

    #[test]
    fn resolves_decimal_and_hex_character_references() {
        assert_eq!(resolve_entity("#8212"), "—");
        assert_eq!(resolve_entity("#x2014"), "—");
        assert_eq!(resolve_entity("#X2014"), "—");
        assert_eq!(resolve_entity("#169"), "©");
    }

    #[test]
    fn resolves_html_named_entities_that_appear_in_real_documents() {
        assert_eq!(resolve_entity("copy"), "©");
        assert_eq!(resolve_entity("nbsp"), "\u{a0}");
        assert_eq!(resolve_entity("rsquo"), "\u{2019}");
        assert_eq!(resolve_entity("hellip"), "…");
    }

    #[test]
    fn unknown_entities_stay_visible_rather_than_vanishing() {
        assert_eq!(resolve_entity("notAnEntity"), "&notAnEntity;");
        // Out-of-range and malformed numeric refs are kept, not silently dropped.
        assert_eq!(resolve_entity("#1114112"), "&#1114112;");
        assert_eq!(resolve_entity("#xZZ"), "&#xZZ;");
    }

    #[test]
    fn surrogate_code_points_are_not_forged_into_chars() {
        // char::from_u32 rejects D800-DFFF; the reference must survive as text.
        assert_eq!(resolve_entity("#xD800"), "&#xD800;");
    }
}
