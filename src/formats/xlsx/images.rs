use crate::entities::read_event_folding_entities;
use std::collections::HashMap;
use std::io::Read;

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub struct SheetImageInfo {
    pub sheet_name: String,
    pub sheet_index: usize,
    pub hash_name: String,
    pub alt_text: Option<String>,
}

/// Returns None for unsupported formats (.emf, .wmf, .tiff, etc.).
/// Returns "{16hexchars}.{ext}" for .png/.jpg/.jpeg/.gif/.webp.
pub fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    let path = zip_path.to_ascii_lowercase();

    crate::image_naming::name_for_path(bytes, &path)
}

fn read_zip_entry(archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> Option<Vec<u8>> {
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn resolve_relative_path(base_dir: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').collect();
    for segment in target.split('/') {
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

fn local_name(name: QName<'_>) -> Vec<u8> {
    let bytes = name.as_ref();
    let idx = bytes
        .iter()
        .rposition(|b| *b == b':')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[idx..].to_vec()
}

fn attr_value(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    String::from_utf8_lossy(attr.value.as_ref()).into_owned()
}

fn parse_sheet_drawing_targets(
    archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>,
    sheet_index_1based: usize,
) -> Vec<String> {
    // .xlsx/.xlsm/.xltx/.xltm store the worksheet as sheetN.xml (rels
    // sheetN.xml.rels); .xlsb stores it as sheetN.bin (rels sheetN.bin.rels).
    // The drawing/media parts are identical XML/binary in both, so trying the
    // .bin.rels fallback lights up xlsb images through this same walker.
    let xml_rels = format!("xl/worksheets/_rels/sheet{}.xml.rels", sheet_index_1based);
    let bin_rels = format!("xl/worksheets/_rels/sheet{}.bin.rels", sheet_index_1based);
    let bytes = match read_zip_entry(archive, &xml_rels)
        .or_else(|| read_zip_entry(archive, &bin_rels))
    {
        Some(b) => b,
        None => return Vec::new(),
    };

    let mut reader = XmlReader::from_reader(bytes.as_slice());
    let mut buf = Vec::new();
    let mut targets = Vec::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Eof) => break,
            Err(_) => return Vec::new(),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"Relationship" {
                    let mut rel_type = String::new();
                    let mut target = String::new();
                    for attr in e.attributes().flatten() {
                        let key = local_name(QName(attr.key.as_ref()));
                        if key.as_slice() == b"Type" {
                            rel_type = attr_value(&attr);
                        } else if key.as_slice() == b"Target" {
                            target = attr_value(&attr);
                        }
                    }
                    if rel_type.ends_with("/drawing") && !target.is_empty() {
                        targets.push(resolve_relative_path("xl/worksheets", &target));
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    targets
}

fn parse_drawing_image_rids(
    archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>,
    drawing_path: &str,
) -> HashMap<String, String> {
    let file_name = drawing_path.rsplit('/').next().unwrap_or("");
    if file_name.is_empty() {
        return HashMap::new();
    }

    let rels_path = format!("xl/drawings/_rels/{}.rels", file_name);
    let bytes = match read_zip_entry(archive, &rels_path) {
        Some(b) => b,
        None => return HashMap::new(),
    };

    let mut reader = XmlReader::from_reader(bytes.as_slice());
    let mut buf = Vec::new();
    let mut images = HashMap::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Eof) => break,
            Err(_) => return HashMap::new(),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"Relationship" {
                    let mut id = String::new();
                    let mut rel_type = String::new();
                    let mut target = String::new();
                    for attr in e.attributes().flatten() {
                        let key = local_name(QName(attr.key.as_ref()));
                        let value = attr_value(&attr);
                        if key.as_slice() == b"Id" {
                            id = value;
                        } else if key.as_slice() == b"Type" {
                            rel_type = value;
                        } else if key.as_slice() == b"Target" {
                            target = value;
                        }
                    }
                    if rel_type.ends_with("/image") && !id.is_empty() && !target.is_empty() {
                        images.insert(id, resolve_relative_path("xl/drawings", &target));
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    images
}

fn extract_drawing_pic_rids(xml_bytes: &[u8]) -> Vec<(Option<String>, Option<String>)> {
    let mut reader = XmlReader::from_reader(std::io::BufReader::new(xml_bytes));
    let mut buf = Vec::new();

    let mut result = Vec::new();
    let mut in_pic = false;
    let mut pic_rid: Option<String> = None;
    let mut pic_alt: Option<String> = None;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) => {
                let raw = e.name().as_ref().to_vec();
                let local: &[u8] = raw.rsplit(|b| *b == b':').next().unwrap_or(&raw);
                match local {
                    b"pic" => {
                        in_pic = true;
                        pic_rid = None;
                        pic_alt = None;
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
                        for attr in e.attributes().flatten() {
                            let ak = attr.key.as_ref().to_vec();
                            let al: &[u8] = ak.rsplit(|b| *b == b':').next().unwrap_or(&ak);
                            if al == b"embed" {
                                let rid = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                if !rid.is_empty() {
                                    pic_rid = Some(rid);
                                }
                                break;
                            }
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
                        for attr in e.attributes().flatten() {
                            let ak = attr.key.as_ref().to_vec();
                            let al: &[u8] = ak.rsplit(|b| *b == b':').next().unwrap_or(&ak);
                            if al == b"embed" {
                                let rid = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                if !rid.is_empty() {
                                    pic_rid = Some(rid);
                                }
                                break;
                            }
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
                }
            }
            _ => {}
        }
        buf.clear();
    }

    result
}

pub fn collect_all_sheet_images(
    data: &[u8],
    workbook_sheet_names: &[String],
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<SheetImageInfo> {
    let mut archive = match ZipArchive::new(std::io::Cursor::new(data.to_vec())) {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };

    let mut result = Vec::new();

    for (sheet_idx, sheet_name) in workbook_sheet_names.iter().enumerate() {
        let drawing_paths = parse_sheet_drawing_targets(&mut archive, sheet_idx + 1);

        for drawing_path in drawing_paths {
            let image_rids = parse_drawing_image_rids(&mut archive, &drawing_path);
            if image_rids.is_empty() {
                continue;
            }

            let xml_bytes = match read_zip_entry(&mut archive, &drawing_path) {
                Some(b) => b,
                None => continue,
            };

            let pic_rids = extract_drawing_pic_rids(&xml_bytes);
            for (rid_opt, alt_opt) in pic_rids {
                let rid = match rid_opt {
                    Some(r) => r,
                    None => continue,
                };
                let zip_path = match image_rids.get(&rid) {
                    Some(p) => p.clone(),
                    None => continue,
                };
                let img_bytes = match read_zip_entry(&mut archive, &zip_path) {
                    Some(b) => b,
                    None => continue,
                };
                let hash_name = match image_hash_name(&img_bytes, &zip_path) {
                    Some(n) => n,
                    None => continue,
                };

                if !image_out.iter().any(|(n, _)| n == &hash_name) {
                    image_out.push((hash_name.clone(), img_bytes));
                }

                result.push(SheetImageInfo {
                    sheet_name: sheet_name.clone(),
                    sheet_index: sheet_idx,
                    hash_name,
                    alt_text: alt_opt,
                });
            }
        }
    }

    result
}

/// Is a `draw:name` an auto-generated placeholder ("Image 1", "Object 2", …)
/// rather than a meaningful caption? Mirrors the OOXML walker's filter.
fn is_generic_draw_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    ["image ", "picture ", "graphic ", "object ", "shape "]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// Extract images from an OpenDocument Spreadsheet (.ods).
///
/// ODS has no OOXML `xl/…` layout: images live in a top-level `Pictures/` folder
/// and are referenced from `content.xml` as
/// `table:table[@table:name] → … → draw:frame[@draw:name] → draw:image[@xlink:href]`.
/// We stream `content.xml`, tracking the current sheet and frame, to reproduce the
/// same per-sheet + alt-text `SheetImageInfo` the OOXML walker yields.
pub fn collect_all_ods_images(
    data: &[u8],
    workbook_sheet_names: &[String],
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<SheetImageInfo> {
    let mut archive = match ZipArchive::new(std::io::Cursor::new(data.to_vec())) {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };

    let content = match read_zip_entry(&mut archive, "content.xml") {
        Some(b) => b,
        None => return Vec::new(),
    };

    // First pass: parse content.xml into (sheet_name, sheet_index, href, alt).
    struct OdsImageRef {
        sheet_name: String,
        sheet_index: usize,
        href: String,
        alt: Option<String>,
    }

    let mut refs: Vec<OdsImageRef> = Vec::new();
    let mut reader = XmlReader::from_reader(content.as_slice());
    let mut buf = Vec::new();

    let mut sheet_counter: usize = 0;
    let mut current_sheet_name = String::new();
    let mut current_sheet_index = 0usize;
    let mut frame_alt: Option<String> = None;
    let mut in_frame = false;
    // Track svg:title / svg:desc text nodes inside a frame (richer than draw:name).
    let mut capture_text_into: Option<&'static str> = None;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = local_name(e.name());
                match name.as_slice() {
                    b"table" => {
                        // table:table — a sheet. Capture its name / index.
                        let mut tname = String::new();
                        for attr in e.attributes().flatten() {
                            if local_name(QName(attr.key.as_ref())).as_slice() == b"name" {
                                tname = attr_value(&attr);
                            }
                        }
                        current_sheet_index = workbook_sheet_names
                            .iter()
                            .position(|n| n == &tname)
                            .unwrap_or(sheet_counter);
                        current_sheet_name = if tname.is_empty() {
                            workbook_sheet_names
                                .get(sheet_counter)
                                .cloned()
                                .unwrap_or_default()
                        } else {
                            tname
                        };
                        sheet_counter += 1;
                    }
                    b"frame" => {
                        in_frame = true;
                        frame_alt = None;
                        for attr in e.attributes().flatten() {
                            if local_name(QName(attr.key.as_ref())).as_slice() == b"name" {
                                let v = attr_value(&attr);
                                if !v.trim().is_empty() && !is_generic_draw_name(&v) {
                                    frame_alt = Some(v);
                                }
                            }
                        }
                    }
                    b"title" | b"desc" if in_frame => {
                        capture_text_into = Some(if name.as_slice() == b"title" {
                            "title"
                        } else {
                            "desc"
                        });
                    }
                    b"image" if in_frame => {
                        // draw:image xlink:href="Pictures/xxx"
                        let mut href = String::new();
                        for attr in e.attributes().flatten() {
                            if local_name(QName(attr.key.as_ref())).as_slice() == b"href" {
                                href = attr_value(&attr);
                            }
                        }
                        if !href.is_empty() {
                            refs.push(OdsImageRef {
                                sheet_name: current_sheet_name.clone(),
                                sheet_index: current_sheet_index,
                                href,
                                alt: frame_alt.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref t)) => {
                if capture_text_into.is_some() && in_frame {
                    let text = String::from_utf8_lossy(t.as_ref()).trim().to_string();
                    if !text.is_empty() {
                        // svg:desc/title is a real caption — prefer it over draw:name.
                        frame_alt = Some(text);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = local_name(e.name());
                match name.as_slice() {
                    b"frame" => {
                        in_frame = false;
                        frame_alt = None;
                    }
                    b"title" | b"desc" => capture_text_into = None,
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    // Second pass: resolve each unique href from the Pictures/ folder, hash it,
    // and build the per-image SheetImageInfo list.
    let mut result = Vec::new();
    for r in refs {
        let img_bytes = match read_zip_entry(&mut archive, &r.href) {
            Some(b) => b,
            None => continue,
        };
        let hash_name = match image_hash_name(&img_bytes, &r.href) {
            Some(n) => n,
            None => continue,
        };
        if !image_out.iter().any(|(n, _)| n == &hash_name) {
            image_out.push((hash_name.clone(), img_bytes));
        }
        result.push(SheetImageInfo {
            sheet_name: r.sheet_name,
            sheet_index: r.sheet_index,
            hash_name,
            alt_text: r.alt,
        });
    }

    result
}

/// Format-aware image collection: ODS uses the ODF walker, every other
/// spreadsheet family uses the OOXML walker (which also handles .xlsb).
pub fn collect_spreadsheet_images(
    data: &[u8],
    ext: &str,
    workbook_sheet_names: &[String],
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> Vec<SheetImageInfo> {
    if ext == "ods" {
        collect_all_ods_images(data, workbook_sheet_names, image_out)
    } else {
        collect_all_sheet_images(data, workbook_sheet_names, image_out)
    }
}
