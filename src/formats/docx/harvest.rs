//! Attribute harvesting from drawing / note / blip XML events.

use super::xml_text::qname_eq;

/// Pull author-provided alt text from a `<wp:docPr>` or `<pic:cNvPr>` start /
/// empty event. Attributes are read in priority order so the most descriptive
/// value wins: `descr` (the "Alt text — Description" field in Word) →
/// `title` (the "Alt text — Title" field) → `name` (the auto-generated image
/// name like `"Picture 3"`, only useful as a last-resort fallback).
/// Returns `None` when no non-empty attribute is found.
pub(super) fn harvest_image_alt(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let mut descr: Option<String> = None;
    let mut title: Option<String> = None;
    let mut name: Option<String> = None;
    for attr in e.attributes().flatten() {
        let value = String::from_utf8_lossy(attr.value.as_ref())
            .trim()
            .to_string();
        if value.is_empty() {
            continue;
        }
        if qname_eq(attr.key, b"descr") {
            descr.get_or_insert(value);
        } else if qname_eq(attr.key, b"title") {
            title.get_or_insert(value);
        } else if qname_eq(attr.key, b"name") {
            // Skip auto-generated names like "Picture 1" / "Image 3" —
            // they add noise without semantic value.
            let lower = value.to_ascii_lowercase();
            let looks_generic = lower.starts_with("picture ")
                || lower.starts_with("image ")
                || lower.starts_with("graphic ")
                || lower.starts_with("chart ");
            if !looks_generic {
                name.get_or_insert(value);
            }
        }
    }
    descr.or(title).or(name)
}

/// Pull the `w:id` attribute from a `<w:footnoteReference>` or
/// `<w:endnoteReference>` event. The id is the document-order link to an
/// entry in `word/footnotes.xml` / `word/endnotes.xml`. Returns `None` when
/// the attribute is missing or empty.
pub(super) fn harvest_note_id(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        if qname_eq(attr.key, b"id") {
            let value = String::from_utf8_lossy(attr.value.as_ref())
                .trim()
                .to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Pull the `r:embed` value from `<a:blip>`.
pub(super) fn harvest_blip_embed(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        if qname_eq(attr.key, b"embed") {
            let value = String::from_utf8_lossy(attr.value.as_ref())
                .trim()
                .to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}
