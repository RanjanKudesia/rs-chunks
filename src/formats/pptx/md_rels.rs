//! Relationship / title / notes lookups for PPTX markdown conversion.

use crate::entities::read_event_folding_entities;
use std::collections::HashMap;
use std::io::Cursor;

use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

use super::common::read_zip_entry;
use super::md_blocks::BlockKind;
use super::md_slide_parse::parse_slide_for_markdown;

/// Extract the presentation title from docProps/core.xml (Dublin Core metadata).
pub(super) fn extract_presentation_title(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
) -> Option<String> {
    use std::io::Read;

    let mut entry = archive.by_name("docProps/core.xml").ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    let xml = std::str::from_utf8(&buf).ok()?;

    let mut reader = XmlReader::from_str(xml);
    let mut event_buf = Vec::new();
    let mut in_title = false;
    let mut title = String::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut event_buf, &mut spill, &mut is_entity) {
            Ok(XmlEvent::Start(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"title" {
                    in_title = true;
                }
            }
            Ok(XmlEvent::Text(ref e)) if in_title => {
                let s = e.decode().unwrap_or_default();
                title = s.trim().to_string();
            }
            Ok(XmlEvent::End(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"title" {
                    in_title = false;
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
        event_buf.clear();
    }

    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// Parse a slide's .rels file and return rId → URL for external hyperlinks.
pub(super) fn parse_slide_rels(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
) -> HashMap<String, String> {
    parse_slide_rels_with_images(archive, slide_name).0
}

pub(super) fn parse_slide_rels_with_images(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
) -> (HashMap<String, String>, HashMap<String, String>) {
    let last_slash = match slide_name.rfind('/') {
        Some(i) => i,
        None => return (HashMap::new(), HashMap::new()),
    };
    let dir = &slide_name[..last_slash];
    let file = &slide_name[last_slash + 1..];
    let rels_path = format!("{}/_rels/{}.rels", dir, file);

    let rels_bytes = match read_zip_entry(archive, &rels_path) {
        Ok(b) => b,
        Err(_) => return (HashMap::new(), HashMap::new()),
    };
    let xml = match std::str::from_utf8(&rels_bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return (HashMap::new(), HashMap::new()),
    };

    let mut hyperlinks = HashMap::new();
    let mut images = HashMap::new();
    let mut reader = XmlReader::from_str(&xml);
    let mut buf = Vec::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(XmlEvent::Empty(ref e)) | Ok(XmlEvent::Start(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"Relationship" {
                    let mut id = String::new();
                    let mut target = String::new();
                    let mut rel_type = String::new();
                    let mut is_external = false;
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        // Same escaping rule as docx: a hyperlink Target goes
                        // straight into `[label](url)`, so it must be decoded.
                        let val = crate::entities::decode_attr(&attr);
                        match key.as_str() {
                            "Id" => id = val,
                            "Target" => target = val,
                            "Type" => rel_type = val,
                            "TargetMode" if val == "External" => is_external = true,
                            _ => {}
                        }
                    }
                    if rel_type.contains("hyperlink")
                        && is_external
                        && !id.is_empty()
                        && !target.is_empty()
                    {
                        hyperlinks.insert(id, target);
                    } else if rel_type.ends_with("/image") && !id.is_empty() && !target.is_empty() {
                        let path = resolve_relative_path(dir, &target);
                        images.insert(id, path);
                    }
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (hyperlinks, images)
}

pub(super) fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    let path = zip_path.to_ascii_lowercase();
    crate::image_naming::name_for_path(bytes, &path)
}

pub(super) fn resolve_relative_path(base_dir: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = base_dir.split('/').collect();
    for segment in relative.split('/') {
        match segment {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub(super) fn extract_notes_text(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
) -> Option<String> {
    // 1. Find notes slide path from <slide_name>.rels
    let last_slash = slide_name.rfind('/')?;
    let dir = &slide_name[..last_slash];
    let file = &slide_name[last_slash + 1..];
    let rels_path = format!("{}/_rels/{}.rels", dir, file);

    let rels_bytes = read_zip_entry(archive, &rels_path).ok()?;
    let content = std::str::from_utf8(&rels_bytes).ok()?;

    // 2. Find Target for notesSlide (not notesSlideLayout)
    let notes_path = content
        .split("<Relationship ")
        .find(|chunk| chunk.contains("notesSlide") && !chunk.contains("notesSlideLayout"))
        .and_then(|chunk| {
            let start = chunk.find("Target=\"")? + 8;
            let rest = &chunk[start..];
            let end = rest.find('"')?;
            let target = crate::entities::decode_attr_value(&rest.as_bytes()[..end]);
            Some(resolve_relative_path(dir, &target))
        })?;

    // 3. Parse notes XML: collect body text (not title which is an image placeholder)
    let notes_buf = read_zip_entry(archive, &notes_path).ok()?;
    let notes_slide = parse_slide_for_markdown(&notes_buf, &HashMap::new()).ok()?;
    let text: String = notes_slide
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Paragraph | BlockKind::ListItem))
        .map(|b| b.text.as_str())
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}
