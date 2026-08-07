//! Namespace-agnostic XML name/attribute helpers.

use quick_xml::events::attributes::Attributes;
use quick_xml::name::QName;

// ── XML helpers ───────────────────────────────────────────────────────────────

pub fn local_name(name: QName<'_>) -> Vec<u8> {
    let raw = name.as_ref();
    raw.rsplit(|b| *b == b':').next().unwrap_or(raw).to_vec()
}

pub fn attr_value(attrs: Attributes<'_>, key: &[u8]) -> Option<String> {
    for attr in attrs.flatten() {
        let aname = attr.key.as_ref();
        let local = aname.rsplit(|b| *b == b':').next().unwrap_or(aname);
        if local == key {
            return attr.unescape_value().ok().map(|v| v.trim().to_string());
        }
    }
    None
}
