//! EPUB OCF container + OPF package parsing.
//!
//! Navigation chain: zip → `META-INF/container.xml` (→ OPF path) → OPF
//! `<manifest>`+`<spine>` → the ordered list of XHTML content documents (the
//! reading order). Works for both EPUB 2 and EPUB 3 (both expose manifest+spine).

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

type Zip = ZipArchive<Cursor<Vec<u8>>>;

pub struct EpubDoc {
    pub href: String,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
pub struct EpubPackage {
    pub title: Option<String>,
    pub language: Option<String>,
    pub creator: Option<String>,
    pub identifier: Option<String>,
    pub version: Option<String>,
    /// XHTML content documents in spine (reading) order.
    pub spine: Vec<EpubDoc>,
    /// Embedded images: full zip path → bytes.
    pub images: Vec<(String, Vec<u8>)>,
}

fn local_name(name: QName<'_>) -> Vec<u8> {
    let b = name.as_ref();
    let idx = b.iter().rposition(|c| *c == b':').map(|i| i + 1).unwrap_or(0);
    b[idx..].to_vec()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if local_name(QName(a.key.as_ref())).as_slice() == key {
            return Some(String::from_utf8_lossy(a.value.as_ref()).into_owned());
        }
    }
    None
}

fn read_entry(zip: &mut Zip, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Resolve an href relative to a base directory (the OPF dir), collapsing
/// `.`/`..` and percent-decoding, into a canonical zip path.
fn resolve_href(base_dir: &str, href: &str) -> String {
    // Drop any fragment.
    let href = href.split('#').next().unwrap_or(href);
    let href = percent_decode(href);
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn unescape_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(h, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Find the OPF path from `META-INF/container.xml`.
fn find_opf_path(zip: &mut Zip) -> Result<String, String> {
    let data = read_entry(zip, "META-INF/container.xml")
        .ok_or_else(|| "Not an EPUB: missing META-INF/container.xml".to_string())?;
    let mut reader = XmlReader::from_reader(data.as_slice());
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Bad container.xml: {e}")),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"rootfile" {
                    if let Some(p) = attr(e, b"full-path") {
                        return Ok(percent_decode(&p));
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Err("Not an EPUB: no rootfile in container.xml".to_string())
}

pub fn parse(file_bytes: Vec<u8>) -> Result<EpubPackage, String> {
    let mut zip = ZipArchive::new(Cursor::new(file_bytes))
        .map_err(|e| format!("Not a valid EPUB (zip) file: {e}"))?;

    let opf_path = find_opf_path(&mut zip)?;
    let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let opf = read_entry(&mut zip, &opf_path)
        .ok_or_else(|| format!("EPUB OPF not found at {opf_path}"))?;

    // ── Parse the OPF: metadata + manifest (id→href,media-type) + spine order ──
    let mut pkg = EpubPackage::default();
    let mut manifest: HashMap<String, (String, String)> = HashMap::new();
    let mut spine_idrefs: Vec<String> = Vec::new();

    let mut reader = XmlReader::from_reader(opf.as_slice());
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut cur_meta: Option<Vec<u8>> = None; // which dc:* element we're inside
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Bad OPF XML: {e}")),
            Ok(Event::Start(ref e)) => {
                let name = local_name(e.name());
                match name.as_slice() {
                    b"package" => pkg.version = attr(e, b"version"),
                    b"title" | b"language" | b"creator" | b"identifier" => {
                        cur_meta = Some(name);
                    }
                    b"item" => {
                        if let (Some(id), Some(href)) = (attr(e, b"id"), attr(e, b"href")) {
                            let mt = attr(e, b"media-type").unwrap_or_default();
                            manifest.insert(id, (href, mt));
                        }
                    }
                    b"itemref" => {
                        if let Some(idref) = attr(e, b"idref") {
                            spine_idrefs.push(idref);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = local_name(e.name());
                match name.as_slice() {
                    b"item" => {
                        if let (Some(id), Some(href)) = (attr(e, b"id"), attr(e, b"href")) {
                            let mt = attr(e, b"media-type").unwrap_or_default();
                            manifest.insert(id, (href, mt));
                        }
                    }
                    b"itemref" => {
                        if let Some(idref) = attr(e, b"idref") {
                            spine_idrefs.push(idref);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(m) = &cur_meta {
                    let text = unescape_entities(&String::from_utf8_lossy(t.as_ref())).trim().to_string();
                    if !text.is_empty() {
                        let slot = match m.as_slice() {
                            b"title" => &mut pkg.title,
                            b"language" => &mut pkg.language,
                            b"creator" => &mut pkg.creator,
                            b"identifier" => &mut pkg.identifier,
                            _ => &mut None,
                        };
                        if slot.is_none() {
                            *slot = Some(text);
                        }
                    }
                }
            }
            Ok(Event::End(_)) => cur_meta = None,
            _ => {}
        }
        buf.clear();
    }

    // ── Resolve spine → ordered XHTML docs; collect images ──
    for idref in &spine_idrefs {
        if let Some((href, _mt)) = manifest.get(idref) {
            let full = resolve_href(&opf_dir, href);
            if let Some(bytes) = read_entry(&mut zip, &full) {
                pkg.spine.push(EpubDoc { href: full, bytes });
            }
        }
    }
    for (href, mt) in manifest.values() {
        if mt.starts_with("image/") {
            let full = resolve_href(&opf_dir, href);
            if let Some(bytes) = read_entry(&mut zip, &full) {
                pkg.images.push((full, bytes));
            }
        }
    }

    if pkg.spine.is_empty() {
        return Err("EPUB has no readable spine content".to_string());
    }
    Ok(pkg)
}
