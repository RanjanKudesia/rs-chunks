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

/// Resolve every entity reference inside a raw XML **attribute** value.
///
/// quick-xml hands attribute values back exactly as they appear on disk:
/// `Target="…?eid=2-s2.0-0024997614&amp;partnerID=K84"` arrives with the
/// `&amp;` still in it. Element *text* has always been decoded — it goes
/// through [`read_event_folding_entities!`] — but attributes had no
/// equivalent, so every OOXML relationship `Target` kept its escapes and
/// `get_markdown` emitted hyperlink URLs containing a literal `&amp;`.
///
/// An attribute value can never legitimately contain a bare `&` (XML forbids
/// it), so decoding here is unconditionally correct rather than a heuristic.
/// A `&` that does *not* start a well-formed reference is passed through
/// untouched, and an unrecognised reference survives as `&name;` — the same
/// "make the gap visible, never delete characters" rule [`resolve_entity`]
/// follows.
pub fn decode_attr_value(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    if !s.contains('&') {
        return s.into_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest: &str = s.as_ref();
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // An entity name is `#`? followed by name characters, then `;`.
        // Anything else is a stray ampersand, kept verbatim.
        let name_end = after
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '#')
            .unwrap_or(after.len());
        if name_end > 0 && after[name_end..].starts_with(';') {
            out.push_str(&resolve_entity(&after[..name_end]));
            rest = &after[name_end + 1..];
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// [`decode_attr_value`] for a quick-xml [`Attribute`](quick_xml::events::attributes::Attribute).
pub fn decode_attr(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    decode_attr_value(attr.value.as_ref())
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

    // ── decode_attr_value ────────────────────────────────────────────────────

    fn decode(s: &str) -> String {
        decode_attr_value(s.as_bytes())
    }

    #[test]
    fn attribute_values_without_an_ampersand_are_returned_unchanged() {
        assert_eq!(decode("word/media/image1.png"), "word/media/image1.png");
        assert_eq!(decode(""), "");
    }

    #[test]
    fn relationship_targets_have_their_ampersands_decoded() {
        // The exact shape of the bug: a Scopus URL out of word/_rels.
        assert_eq!(
            decode(
                "http://www.scopus.com/record.url?eid=2-s2.0-00249&amp;partnerID=K84&amp;rel=3.0.0"
            ),
            "http://www.scopus.com/record.url?eid=2-s2.0-00249&partnerID=K84&rel=3.0.0"
        );
    }

    #[test]
    fn attribute_values_decode_the_same_table_element_text_does() {
        assert_eq!(decode("&lt;b&gt;"), "<b>");
        assert_eq!(decode("&quot;quoted&quot;"), "\"quoted\"");
        assert_eq!(decode("R&amp;D &#8212; &copy;"), "R&D — ©");
    }

    #[test]
    fn a_stray_ampersand_is_kept_rather_than_swallowed() {
        // Not well-formed XML, but real files contain it; never delete data.
        assert_eq!(decode("a & b"), "a & b");
        assert_eq!(decode("q?a=1&b=2"), "q?a=1&b=2");
        assert_eq!(decode("trailing&"), "trailing&");
    }

    #[test]
    fn unknown_references_in_attributes_stay_visible() {
        assert_eq!(decode("&notAnEntity;"), "&notAnEntity;");
    }
}
