//! EPUB OCF container + OPF package parsing.
//!
//! Navigation chain: zip → `META-INF/container.xml` (→ OPF path) → OPF
//! `<manifest>`+`<spine>` → the ordered list of XHTML content documents (the
//! reading order). Works for both EPUB 2 and EPUB 3 (both expose manifest+spine).

use crate::entities::read_event_folding_entities;
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
    /// This spine document is the book's navigation, not its content.
    ///
    /// A TOC's link list chunked as `short_disconnected_paragraph` is
    /// indistinguishable from real prose and pollutes retrieval — every book
    /// contributes a chunk that is just chapter names. (#40)
    pub is_navigation: bool,
}

#[derive(Default)]
pub struct EpubPackage {
    /// Spine items the container does not actually hold, in reading order.
    ///
    /// A missing zip entry, or an `itemref` whose `idref` has no manifest
    /// `item`, used to be dropped silently — and `spine_count` then reported
    /// the shortened list as the book's length, so the loss was not merely
    /// invisible, it was actively asserted away. Always present, empty when
    /// nothing was lost, so its absence never has to be interpreted; the same
    /// contract as xlsx's `skipped_sheets` (#66).
    pub skipped_spine_items: Vec<String>,
    pub title: Option<String>,
    pub language: Option<String>,
    pub creator: Option<String>,
    pub identifier: Option<String>,
    pub version: Option<String>,
    /// Every value of each repeatable Dublin Core element, in document order.
    ///
    /// The singular fields above keep the FIRST value, which is what they
    /// always held — changing their type would break consumers. But collapsing
    /// to the first is real loss: `tika_testEPUB_multi-metadata-vals.epub` has
    /// two authors, two identifiers, two publishers, two languages and **eight
    /// contributors**, and contributors were not surfaced at all. (#38)
    pub creators: Vec<String>,
    pub identifiers: Vec<String>,
    pub publishers: Vec<String>,
    pub contributors: Vec<String>,
    pub subjects: Vec<String>,
    pub languages: Vec<String>,
    /// The book's own table of contents: `(title, href)` in reading order,
    /// from `nav.xhtml` (EPUB 3) or `toc.ncx` (EPUB 2). Never parsed before, so
    /// chapter titles surfaced only when the HTML happened to use headings. (#39)
    pub toc: Vec<(String, String)>,
    /// XHTML content documents in spine (reading) order.
    pub spine: Vec<EpubDoc>,
    /// Embedded images: full zip path → bytes.
    pub images: crate::chunk::ExtractedImages,
}

/// A book's own table of contents, from `nav.xhtml` (EPUB 3) or `toc.ncx`
/// (EPUB 2). Returns `(title, href)` in document order.
///
/// Both formats bury the same two facts in different places: NCX puts the label
/// in `<navLabel><text>` and the target in `<content src>`; the XHTML nav puts
/// both on an `<a href>`. Handled with one walker rather than two.
fn parse_toc(xml: &[u8]) -> Vec<(String, String)> {
    let mut reader = XmlReader::from_reader(std::io::BufReader::new(xml));
    let mut buf = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();

    let mut in_text = false; // <text> (ncx) or <a> (xhtml)
    let mut label = String::new();
    let mut pending_src: Option<String> = None;
    let mut anchor_href: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                match local_name(e.name()).as_slice() {
                    b"text" => {
                        in_text = true;
                        label.clear();
                    }
                    b"a" => {
                        in_text = true;
                        label.clear();
                        anchor_href = attr(e, b"href");
                    }
                    b"content" => pending_src = attr(e, b"src"),
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) if in_text => {
                label.push_str(e.decode().unwrap_or_default().as_ref());
            }
            Ok(Event::End(ref e)) => match local_name(e.name()).as_slice() {
                b"a" => {
                    in_text = false;
                    let title = label.trim().to_string();
                    if let (false, Some(href)) = (title.is_empty(), anchor_href.take()) {
                        out.push((title, href));
                    }
                }
                b"text" => in_text = false,
                b"navPoint" => {
                    let title = label.trim().to_string();
                    if let (false, Some(src)) = (title.is_empty(), pending_src.take()) {
                        out.push((title, src));
                    }
                    label.clear();
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}

/// EPUB 3 marks its navigation document with `properties="nav"`. EPUB 2 has no
/// such marker — the TOC is just another spine document — so fall back to the
/// naming convention every EPUB 2 producer follows. A heuristic, and labelled
/// as one: it only sets a metadata flag, never drops content. (#40)
fn looks_like_navigation(id: &str, href: &str) -> bool {
    let stem = href
        .rsplit('/')
        .next()
        .unwrap_or(href)
        .split('.')
        .next()
        .unwrap_or("");
    [id, stem].iter().any(|s| {
        let squashed: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        matches!(
            squashed.as_str(),
            "toc" | "tableofcontents" | "contents" | "nav" | "navigation"
        )
    })
}

fn local_name(name: QName<'_>) -> Vec<u8> {
    let b = name.as_ref();
    let idx = b
        .iter()
        .rposition(|c| *c == b':')
        .map(|i| i + 1)
        .unwrap_or(0);
    b[idx..].to_vec()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if local_name(QName(a.key.as_ref())).as_slice() == key {
            // Attribute values are escaped: an `href` naming a part with "&" in
            // it is written "&amp;" and will not open until it is decoded.
            return Some(crate::entities::decode_attr(&a));
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
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Bad container.xml: {e}")),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if local_name(e.name()).as_slice() == b"rootfile" =>
            {
                if let Some(p) = attr(e, b"full-path") {
                    return Ok(percent_decode(&p));
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
    let opf_dir = opf_path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    let opf = read_entry(&mut zip, &opf_path)
        .ok_or_else(|| format!("EPUB OPF not found at {opf_path}"))?;

    // ── Parse the OPF: metadata + manifest (id→href,media-type) + spine order ──
    let mut pkg = EpubPackage::default();
    let mut manifest: HashMap<String, (String, String, String)> = HashMap::new();
    let mut spine_idrefs: Vec<String> = Vec::new();

    let mut reader = XmlReader::from_reader(opf.as_slice());
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut cur_meta: Option<Vec<u8>> = None; // which dc:* element we're inside
    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Bad OPF XML: {e}")),
            Ok(Event::Start(ref e)) => {
                let name = local_name(e.name());
                match name.as_slice() {
                    b"package" => pkg.version = attr(e, b"version"),
                    b"title" | b"language" | b"creator" | b"identifier" | b"publisher"
                    | b"contributor" | b"subject" => {
                        cur_meta = Some(name);
                    }
                    b"item" => {
                        if let (Some(id), Some(href)) = (attr(e, b"id"), attr(e, b"href")) {
                            let mt = attr(e, b"media-type").unwrap_or_default();
                            let props = attr(e, b"properties").unwrap_or_default();
                            manifest.insert(id, (href, mt, props));
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
                            let props = attr(e, b"properties").unwrap_or_default();
                            manifest.insert(id, (href, mt, props));
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
                    let text = unescape_entities(&String::from_utf8_lossy(t.as_ref()))
                        .trim()
                        .to_string();
                    if !text.is_empty() {
                        let slot = match m.as_slice() {
                            b"title" => &mut pkg.title,
                            b"language" => &mut pkg.language,
                            b"creator" => &mut pkg.creator,
                            b"identifier" => &mut pkg.identifier,
                            _ => &mut None,
                        };
                        // Every occurrence goes into the list; the singular
                        // field keeps the first, as it always did. (#38)
                        let list = match m.as_slice() {
                            b"creator" => Some(&mut pkg.creators),
                            b"identifier" => Some(&mut pkg.identifiers),
                            b"publisher" => Some(&mut pkg.publishers),
                            b"contributor" => Some(&mut pkg.contributors),
                            b"subject" => Some(&mut pkg.subjects),
                            b"language" => Some(&mut pkg.languages),
                            _ => None,
                        };
                        if let Some(list) = list {
                            if !list.iter().any(|v| v == &text) {
                                list.push(text.clone());
                            }
                        }
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
        // One dangling href must not lose the book — an 800-chunk EPUB with a
        // single missing chapter is still overwhelmingly worth returning — but
        // it must not be invisible either. Isolate and record, like a skipped
        // sheet.
        let Some((href, _mt, props)) = manifest.get(idref) else {
            pkg.skipped_spine_items
                .push(format!("{idref} (no manifest item)"));
            continue;
        };
        let full = resolve_href(&opf_dir, href);
        let is_navigation =
            props.split_whitespace().any(|p| p == "nav") || looks_like_navigation(idref, href);
        match read_entry(&mut zip, &full) {
            Some(bytes) => pkg.spine.push(EpubDoc {
                href: full,
                bytes,
                is_navigation,
            }),
            None => pkg.skipped_spine_items.push(full),
        }
    }

    // The TOC lives in the nav document (EPUB 3) or the NCX (EPUB 2). (#39)
    let toc_href = manifest
        .values()
        .find(|(_, _, props)| props.split_whitespace().any(|p| p == "nav"))
        .or_else(|| {
            manifest
                .values()
                .find(|(_, mt, _)| mt == "application/x-dtbncx+xml")
        })
        .map(|(href, _, _)| resolve_href(&opf_dir, href));
    if let Some(href) = toc_href {
        if let Some(bytes) = read_entry(&mut zip, &href) {
            pkg.toc = parse_toc(&bytes);
        }
    }

    for (href, mt, _props) in manifest.values() {
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
