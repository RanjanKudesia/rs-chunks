//! Slide image relationship parsing and image extraction.

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::entities::read_event_folding_entities;

use super::archive::{read_zip_entry, resolve_relative_path, PptxArchive};

// ── Image extraction helpers ──────────────────────────────────────────────────

/// Returns `None` for unsupported formats (.emf, .wmf, etc.).
/// Returns `"<16hexchars>.<ext>"` for .png/.jpg/.jpeg/.gif/.webp.
pub fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    
    let path = zip_path.to_ascii_lowercase();
    crate::image_naming::name_for_path(bytes, &path)
}

/// Parse a slide's .rels file and return rId → zip_path for image relationships only.
/// E.g. `"rId2"` → `"ppt/media/image4.jpeg"`.
pub fn parse_slide_image_rids(
    archive: &mut PptxArchive,
    slide_name: &str,
) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let last_slash = match slide_name.rfind('/') {
        Some(i) => i,
        None => return HashMap::new(),
    };
    let dir = &slide_name[..last_slash];
    let file = &slide_name[last_slash + 1..];
    let rels_path = format!("{}/_rels/{}.rels", dir, file);

    let rels_bytes = match read_zip_entry(archive, &rels_path) {
        Ok(b) => b,
        Err(_) => return HashMap::new(),
    };
    let content = match std::str::from_utf8(&rels_bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return HashMap::new(),
    };

    let mut images = HashMap::new();
    for chunk in content.split("<Relationship ") {
        if !chunk.contains("/image") {
            continue;
        }
        let id = extract_attr(chunk, "Id");
        let target = extract_attr(chunk, "Target");
        if let (Some(id), Some(target)) = (id, target) {
            let zip_path = resolve_relative_path(dir, &target);
            images.insert(id, zip_path);
        }
    }
    images
}

/// Extract attribute value from a `<Relationship .../>` chunk (simple string parsing).
fn extract_attr(chunk: &str, attr: &str) -> Option<String> {
    let needle = format!("{}=\"", attr);
    let start = chunk.find(&needle)? + needle.len();
    let rest = &chunk[start..];
    let end = rest.find('"')?;
    let val = rest[..end].trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

/// Scan a slide's XML for `<p:pic>` elements.
/// Returns `Vec<(rId, alt_text)>` — one entry per picture found.
/// `rId` comes from `<a:blip r:embed="rIdN"/>`.
/// `alt_text` comes from `<p:cNvPr descr="..."/>` (or `name=` as fallback).
/// `r:embed` of an `<a:blip>`, i.e. the relationship id of the image it draws.
fn blip_embed_rid(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        let ak = attr.key.as_ref().to_vec();
        let al: &[u8] = ak.rsplit(|b| *b == b':').next().unwrap_or(&ak);
        if al == b"embed" {
            let rid = String::from_utf8_lossy(attr.value.as_ref()).trim().to_string();
            return if rid.is_empty() { None } else { Some(rid) };
        }
    }
    None
}

pub fn extract_slide_pic_rids(xml_bytes: &[u8]) -> Vec<(Option<String>, Option<String>)> {
    let mut reader = Reader::from_reader(std::io::BufReader::new(xml_bytes));
    let mut buf = Vec::new();
    let mut result = Vec::new();
    let mut in_pic = false;
    let mut pic_rid: Option<String> = None;
    let mut pic_alt: Option<String> = None;
    let mut in_bg = false;
    let mut bg_rid: Option<String> = None;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Start(ref e)) => {
                let raw = e.name().as_ref().to_vec();
                let local: &[u8] = raw.rsplit(|b| *b == b':').next().unwrap_or(&raw);
                match local {
                    b"pic" => {
                        in_pic = true;
                        pic_rid = None;
                        pic_alt = None;
                    }
                    b"bg" => {
                        in_bg = true;
                        bg_rid = None;
                    }
                    b"cNvPr" if in_pic && pic_alt.is_none() => {
                        let mut descr: Option<String> = None;
                        let mut name_val: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            let ak = attr.key.as_ref().to_vec();
                            let al: &[u8] = ak.rsplit(|b| *b == b':').next().unwrap_or(&ak);
                            let v = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            if !v.is_empty() {
                                match al {
                                    b"descr" => descr = Some(v),
                                    b"name" => name_val = Some(v),
                                    _ => {}
                                }
                            }
                        }
                        if let Some(d) = descr {
                            pic_alt = Some(d);
                        } else if let Some(n) = name_val {
                            let lower = n.to_ascii_lowercase();
                            let generic = lower.starts_with("picture ")
                                || lower.starts_with("image ")
                                || lower.starts_with("graphic ")
                                || lower.starts_with("content placeholder");
                            if !generic {
                                pic_alt = Some(n);
                            }
                        }
                    }
                    b"blip" if in_pic => {
                        if let Some(rid) = blip_embed_rid(e) {
                            pic_rid = Some(rid);
                        }
                    }
                    // A slide background is drawn by <p:bg><p:bgPr><a:blipFill>
                    // — never inside a <p:pic>, so the `in_pic` gate above
                    // skipped it and background-only slides yielded no images
                    // at all (TECH_DEBT #17). There is no <p:cNvPr> here, so
                    // there is no alt text to carry.
                    b"blip" if in_bg => {
                        if let Some(rid) = blip_embed_rid(e) {
                            bg_rid = Some(rid);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let raw = e.name().as_ref().to_vec();
                let local: &[u8] = raw.rsplit(|b| *b == b':').next().unwrap_or(&raw);
                match local {
                    b"pic" => {
                        // self-closing <p:pic/> — rare but handle it
                        result.push((None, None));
                    }
                    b"cNvPr" if in_pic && pic_alt.is_none() => {
                        let mut descr: Option<String> = None;
                        let mut name_val: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            let ak = attr.key.as_ref().to_vec();
                            let al: &[u8] = ak.rsplit(|b| *b == b':').next().unwrap_or(&ak);
                            let v = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            if !v.is_empty() {
                                match al {
                                    b"descr" => descr = Some(v),
                                    b"name" => name_val = Some(v),
                                    _ => {}
                                }
                            }
                        }
                        if let Some(d) = descr {
                            pic_alt = Some(d);
                        } else if let Some(n) = name_val {
                            let lower = n.to_ascii_lowercase();
                            let generic = lower.starts_with("picture ")
                                || lower.starts_with("image ")
                                || lower.starts_with("graphic ")
                                || lower.starts_with("content placeholder");
                            if !generic {
                                pic_alt = Some(n);
                            }
                        }
                    }
                    b"blip" if in_pic => {
                        if let Some(rid) = blip_embed_rid(e) {
                            pic_rid = Some(rid);
                        }
                    }
                    // A slide background is drawn by <p:bg><p:bgPr><a:blipFill>
                    // — never inside a <p:pic>, so the `in_pic` gate above
                    // skipped it and background-only slides yielded no images
                    // at all (TECH_DEBT #17). There is no <p:cNvPr> here, so
                    // there is no alt text to carry.
                    b"blip" if in_bg => {
                        if let Some(rid) = blip_embed_rid(e) {
                            bg_rid = Some(rid);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let raw = e.name().as_ref().to_vec();
                let local: &[u8] = raw.rsplit(|b| *b == b':').next().unwrap_or(&raw);
                if local == b"pic" && in_pic {
                    in_pic = false;
                    result.push((pic_rid.take(), pic_alt.take()));
                } else if local == b"bg" && in_bg {
                    in_bg = false;
                    if let Some(rid) = bg_rid.take() {
                        result.push((Some(rid), None));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    result
}

pub struct SlideImageInfo {
    pub slide_num: usize,
    pub hash_name: String,
    pub alt_text: Option<String>,
}

/// Collect all extractable images from all slides.
/// Populates `image_out` with deduplicated (hash_name, bytes) pairs.
/// Returns per-image-chunk info (one entry per image occurrence, including duplicates across slides).
pub fn collect_all_slide_images(
    archive: &mut PptxArchive,
    slide_names: &[(usize, String)],
    _total_slides: usize,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<SlideImageInfo> {
    let mut result = Vec::new();

    for (slide_num, slide_name) in slide_names {
        let image_rids = parse_slide_image_rids(archive, slide_name);
        if image_rids.is_empty() {
            continue;
        }

        let xml_bytes = match read_zip_entry(archive, slide_name) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let pic_rids = extract_slide_pic_rids(&xml_bytes);

        for (rid_opt, alt_opt) in pic_rids {
            let rid = match rid_opt {
                Some(r) => r,
                None => continue,
            };
            let zip_path = match image_rids.get(&rid) {
                Some(p) => p.clone(),
                None => continue,
            };
            let img_bytes = match read_zip_entry(archive, &zip_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let hash_name = match image_hash_name(&img_bytes, &zip_path) {
                Some(n) => n,
                None => continue,
            };
            if !image_out.iter().any(|(n, _)| n == &hash_name) {
                image_out.push((hash_name.clone(), img_bytes));
            }
            result.push(SlideImageInfo {
                slide_num: *slide_num,
                hash_name,
                alt_text: alt_opt,
            });
        }
    }
    result
}
