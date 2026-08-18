//! Image placeholders, `document.xml.rels` parsing and image-chunk assembly.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::entities::read_event_folding_entities;

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

/// Render the placeholder string used in place of an image-only paragraph's
/// body. Includes the harvested alt text when available so downstream
/// consumers (embedders, search) get a real semantic signal instead of an
/// opaque marker.
pub(super) fn image_placeholder(alt: Option<&str>) -> String {
    match alt.map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => format!("[Image: {a}]"),
        None => "[Image]".to_string(),
    }
}

/// A paragraph's chunk text, keeping **both** its words and its image marker.
///
/// `get_markdown` emits the text and then the placeholder ([#2](TECH_DEBT.md)).
/// `get_chunks` did the opposite of what markdown used to do — it kept the text
/// and dropped the marker — so a paragraph holding both lost its alt text from
/// the chunks entirely, while a paragraph holding *only* an image did get
/// `[Image: …]`. Chunks disagreed with markdown and with themselves
/// ([#83](TECH_DEBT.md)).
pub(super) fn text_with_image_marker(text: String, has_drawing: bool, alt: Option<&str>) -> String {
    if !has_drawing {
        return text;
    }
    let placeholder = image_placeholder(alt);
    if text.trim().is_empty() {
        return placeholder;
    }
    format!("{text}\n{placeholder}")
}

/// Parse `word/_rels/document.xml.rels` and return a map of `rId → zip path`
/// for image relationships (Type ending in `/image`).
pub(super) fn parse_rels_xml_images(xml: &str) -> HashMap<String, String> {
    let mut images = HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"Relationship" {
                    let mut id = String::new();
                    let mut target = String::new();
                    let mut rel_type = String::new();

                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        // Escaped like every attribute value: a part name holding
                        // "&" is written "&amp;" and must be decoded before it can
                        // be looked up in the zip.
                        let val = crate::entities::decode_attr(&attr);
                        match key.as_str() {
                            "Id" => id = val,
                            "Target" => target = val,
                            "Type" => rel_type = val,
                            _ => {}
                        }
                    }

                    if rel_type.ends_with("/image") && !id.is_empty() && !target.is_empty() {
                        let normalized = if let Some(stripped) = target.strip_prefix("../") {
                            format!("word/{stripped}")
                        } else if target.starts_with("word/") {
                            target
                        } else {
                            format!("word/{target}")
                        };
                        images.insert(id, normalized);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    images
}

/// Hash image bytes and return `"<16hex>.<ext>"` or `None` for unsupported
/// formats (`.emf`, `.wmf`, etc.).
pub(super) fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    let path = zip_path.to_ascii_lowercase();
    crate::image_naming::name_for_path(bytes, &path)
}

/// An opened DOCX archive paired with its `r:embed` → image-part-path map.
pub(super) type DocxArchiveWithRids = (
    zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
    std::collections::HashMap<String, String>,
);

/// Open a DOCX zip from bytes and read the `r:embed` → image-part-path map from
/// `word/_rels/document.xml.rels`. Shared by the `*_with_images` chunkers.
pub(super) fn open_docx_archive_with_rids(bytes: &[u8]) -> Result<DocxArchiveWithRids, String> {
    use std::io::Read;
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Not a valid DOCX ZIP: {e}"))?;
    let image_rids_map = match archive.by_name("word/_rels/document.xml.rels") {
        Ok(mut f) => {
            let mut xml = String::new();
            let _ = f.read_to_string(&mut xml);
            parse_rels_xml_images(&xml)
        }
        Err(_) => std::collections::HashMap::new(),
    };
    Ok((archive, image_rids_map))
}

/// From a list of `(rid, alt)` image references, extract deduped image bytes and
/// build `(hash_name, {image_name, alt_text})` chunk entries. Shared by the
/// item-splitting `*_with_images` chunkers (sentence/page_aware/sliding_window).
pub(super) fn collect_image_chunks_from_items(
    image_items: Vec<(Option<String>, Option<String>)>,
    image_rids_map: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
) -> (
    Vec<(String, serde_json::Value)>,
    crate::chunk::ExtractedImages,
) {
    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    let mut entries: Vec<(String, serde_json::Value)> = Vec::new();
    for (rid, alt) in image_items {
        let Some(rid) = rid else { continue };
        let Some(zip_path) = image_rids_map.get(&rid) else {
            continue;
        };
        if let Ok(mut entry) = archive.by_name(zip_path) {
            let mut img_bytes = Vec::new();
            if entry.read_to_end(&mut img_bytes).is_ok() {
                if let Some(hash_name) = image_hash_name(&img_bytes, zip_path) {
                    if !image_out.iter().any(|(n, _)| n == &hash_name) {
                        image_out.push((hash_name.clone(), img_bytes));
                    }
                    let alt_str = alt.as_deref().unwrap_or("");
                    entries.push((
                        hash_name.clone(),
                        serde_json::json!({ "image_name": hash_name, "alt_text": alt_str }),
                    ));
                }
            }
        }
    }
    (entries, image_out)
}
