use crate::entities::read_event_folding_entities;
use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

use super::common::{
    docx_heading_level, image_hash_name, image_placeholder, parse_docx_blocks,
    parse_rels_xml_images, DocxBlock, DocxBlockKind,
};
use super::structural::{extract_notes_map, read_zip_entry};

const MAX_AUX_XML_BYTES: u64 = 10 * 1024 * 1024;

// ── numbering.xml parser ──────────────────────────────────────────────────────

/// Parse `word/numbering.xml` and return a map of `num_id → is_ordered`.
/// Ordered = any numFmt at ilvl 0 other than "bullet" or "none".
fn parse_numbering_xml(xml: &str) -> HashMap<u32, bool> {
    let mut abstract_ordered: HashMap<u32, bool> = HashMap::new();
    let mut num_to_abstract: HashMap<u32, u32> = HashMap::new();

    let mut reader = XmlReader::from_str(xml);
    let mut buf = Vec::new();

    let mut cur_abstract_id: Option<u32> = None;
    let mut cur_abstract_ordered = false;
    let mut in_lvl0 = false;
    let mut cur_num_id: Option<u32> = None;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        let event = read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity);
        match event {
            Ok(XmlEvent::Start(ref e)) | Ok(XmlEvent::Empty(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);

                match local {
                    b"abstractNum" => {
                        cur_abstract_id = None;
                        cur_abstract_ordered = false;
                        in_lvl0 = false;
                        for attr in e.attributes().flatten() {
                            let key_local: &[u8] = attr
                                .key
                                .as_ref()
                                .rsplit(|b| *b == b':')
                                .next()
                                .unwrap_or(attr.key.as_ref());
                            if key_local == b"abstractNumId" {
                                if let Ok(v) = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .parse::<u32>()
                                {
                                    cur_abstract_id = Some(v);
                                }
                                break;
                            }
                        }
                    }
                    b"lvl" => {
                        in_lvl0 = false;
                        for attr in e.attributes().flatten() {
                            let key_local: &[u8] = attr
                                .key
                                .as_ref()
                                .rsplit(|b| *b == b':')
                                .next()
                                .unwrap_or(attr.key.as_ref());
                            if key_local == b"ilvl" {
                                in_lvl0 =
                                    String::from_utf8_lossy(attr.value.as_ref()).trim() == "0";
                                break;
                            }
                        }
                    }
                    b"numFmt" if in_lvl0 => {
                        for attr in e.attributes().flatten() {
                            let key_local: &[u8] = attr
                                .key
                                .as_ref()
                                .rsplit(|b| *b == b':')
                                .next()
                                .unwrap_or(attr.key.as_ref());
                            if key_local == b"val" {
                                let v = String::from_utf8_lossy(attr.value.as_ref());
                                let fmt = v.trim();
                                cur_abstract_ordered = fmt != "bullet" && fmt != "none";
                                break;
                            }
                        }
                    }
                    b"num" => {
                        cur_num_id = None;
                        for attr in e.attributes().flatten() {
                            let key_local: &[u8] = attr
                                .key
                                .as_ref()
                                .rsplit(|b| *b == b':')
                                .next()
                                .unwrap_or(attr.key.as_ref());
                            if key_local == b"numId" {
                                if let Ok(v) = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .parse::<u32>()
                                {
                                    cur_num_id = Some(v);
                                }
                                break;
                            }
                        }
                    }
                    b"abstractNumId" => {
                        if let Some(num_id) = cur_num_id {
                            for attr in e.attributes().flatten() {
                                let key_local: &[u8] = attr
                                    .key
                                    .as_ref()
                                    .rsplit(|b| *b == b':')
                                    .next()
                                    .unwrap_or(attr.key.as_ref());
                                if key_local == b"val" {
                                    if let Ok(abs_id) = String::from_utf8_lossy(attr.value.as_ref())
                                        .trim()
                                        .parse::<u32>()
                                    {
                                        num_to_abstract.insert(num_id, abs_id);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::End(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                match local {
                    b"abstractNum" => {
                        if let Some(id) = cur_abstract_id {
                            abstract_ordered.insert(id, cur_abstract_ordered);
                        }
                        cur_abstract_id = None;
                        in_lvl0 = false;
                    }
                    b"lvl" => {
                        in_lvl0 = false;
                    }
                    b"num" => {
                        cur_num_id = None;
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    num_to_abstract
        .into_iter()
        .map(|(num_id, abs_id)| {
            let ordered = abstract_ordered.get(&abs_id).copied().unwrap_or(false);
            (num_id, ordered)
        })
        .collect()
}

// ── _rels/document.xml.rels parser ───────────────────────────────────────────

/// Parse `word/_rels/document.xml.rels` and return hyperlink map plus header paths.
fn parse_rels_xml(xml: &str) -> (HashMap<String, String>, Vec<String>) {
    let mut hyperlinks = HashMap::new();
    let mut header_paths: Vec<String> = Vec::new();
    let mut reader = XmlReader::from_str(xml);
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
                        let val = String::from_utf8_lossy(attr.value.as_ref()).to_string();
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
                    } else if rel_type.ends_with("/header") && !target.is_empty() {
                        // Target is relative to word/, e.g. "header1.xml"
                        let path = if target.starts_with("word/") {
                            target
                        } else {
                            format!("word/{}", target)
                        };
                        if !header_paths.contains(&path) {
                            header_paths.push(path);
                        }
                    }
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    header_paths.sort();
    (hyperlinks, header_paths)
}

/// Extract plain text from a DOCX header/footer XML file.
/// Concatenates all <w:t> text nodes, separated by spaces.
fn extract_header_text(xml: &str) -> String {
    let mut reader = XmlReader::from_str(xml);
    let mut buf = Vec::new();
    let mut text = String::new();
    let mut in_t = false;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(XmlEvent::Start(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"t" {
                    in_t = true;
                }
            }
            Ok(XmlEvent::End(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"t" {
                    in_t = false;
                }
            }
            Ok(XmlEvent::Text(ref e)) if in_t => {
                let s = e.decode().unwrap_or_default();
                let s = s.trim();
                if !s.is_empty() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(s);
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    text.trim().to_string()
}

// ── Markdown serialiser ───────────────────────────────────────────────────────

fn blocks_to_markdown(
    blocks: &[DocxBlock],
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
    num_id_map: &HashMap<u32, bool>,
    rels_map: &HashMap<String, String>,
) -> String {
    let mut out = String::new();
    let mut footnote_queue: Vec<(String, String)> = Vec::new();
    // Counter per (num_id, list_level) for ordered lists.
    let mut list_counters: HashMap<(u32, u8), usize> = HashMap::new();
    let mut prev_was_list = false;

    for block in blocks {
        match block.kind {
            DocxBlockKind::Table => {
                if !block.text.trim().is_empty() {
                    if prev_was_list {
                        out.push('\n');
                    }
                    out.push('\n');
                    out.push_str(block.text.trim());
                    out.push_str("\n\n");
                }
                list_counters.clear();
                prev_was_list = false;
            }
            DocxBlockKind::Paragraph => {
                let raw = block.text.trim();

                if raw.is_empty() && !block.has_drawing {
                    continue;
                }

                // Collect footnote/endnote refs
                for id in &block.footnote_refs {
                    if let Some(t) = footnote_map.get(id) {
                        footnote_queue.push((id.clone(), t.clone()));
                    }
                }
                for id in &block.endnote_refs {
                    if let Some(t) = endnote_map.get(id) {
                        footnote_queue.push((id.clone(), t.clone()));
                    }
                }

                // Apply hyperlink substitutions: replace anchor text with [text](url)
                let text = if !block.hyperlinks.is_empty() {
                    let mut t = raw.to_string();
                    for (anchor, rid) in &block.hyperlinks {
                        if let Some(url) = rels_map.get(rid) {
                            let a = anchor.trim();
                            if !a.is_empty() {
                                t = t.replacen(a, &format!("[{}]({})", a, url), 1);
                            }
                        }
                    }
                    t
                } else {
                    raw.to_string()
                };

                let heading_level =
                    docx_heading_level(block.heading_style.as_deref(), block.outline_level);

                if let Some(level) = heading_level {
                    if prev_was_list {
                        out.push('\n');
                    }
                    list_counters.clear();
                    let hashes = "#".repeat((level as usize).min(6));
                    out.push_str(&format!("{} {}\n\n", hashes, text));
                    prev_was_list = false;
                } else if block.has_drawing {
                    if prev_was_list {
                        out.push('\n');
                    }
                    list_counters.clear();
                    let placeholder = image_placeholder(block.image_alt.as_deref());
                    out.push_str(&format!("{}\n\n", placeholder));
                    prev_was_list = false;
                } else if block.is_list {
                    let indent = "  ".repeat(block.list_level as usize);
                    // Primary: num_id → numbering.xml lookup.
                    // Fallback: infer from style name when num_id is absent
                    // (python-docx and some templates omit inline <w:numPr>).
                    let is_ordered = block
                        .num_id
                        .and_then(|id| num_id_map.get(&id))
                        .copied()
                        .unwrap_or_else(|| {
                            let s = block
                                .heading_style
                                .as_deref()
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            s.contains("number") && !s.contains("unnumber")
                        });

                    if is_ordered {
                        let key = (block.num_id.unwrap_or(0), block.list_level);
                        let counter = list_counters.entry(key).or_insert(0);
                        *counter += 1;
                        out.push_str(&format!("{}{}. {}\n", indent, counter, text));
                    } else {
                        out.push_str(&format!("{}- {}\n", indent, text));
                    }
                    prev_was_list = true;
                } else {
                    if prev_was_list {
                        out.push('\n');
                    }
                    list_counters.clear();
                    let style_lc = block
                        .heading_style
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if style_lc.contains("code") || text.contains("```") {
                        out.push_str(&format!("```\n{}\n```\n\n", text));
                    } else {
                        if block.page_break || block.section_break {
                            out.push_str("---\n\n");
                        }
                        out.push_str(&text);
                        out.push_str("\n\n");
                    }
                    prev_was_list = false;
                }
            }
        }
    }

    // Append footnotes/endnotes
    if !footnote_queue.is_empty() {
        out.push_str("---\n\n");
        for (id, text) in &footnote_queue {
            out.push_str(&format!("[^{}]: {}\n", id, text));
        }
        out.push('\n');
    }

    // Collapse excess blank lines
    let mut result = String::with_capacity(out.len());
    let mut blank_count = 0u32;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }

    result.trim_end().to_string()
}

fn blocks_to_markdown_with_images(
    blocks: &[DocxBlock],
    footnote_map: &HashMap<String, String>,
    endnote_map: &HashMap<String, String>,
    num_id_map: &HashMap<u32, bool>,
    rels_map: &HashMap<String, String>,
    image_rids_map: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> String {
    let mut out = String::new();
    let mut footnote_queue: Vec<(String, String)> = Vec::new();
    let mut list_counters: HashMap<(u32, u8), usize> = HashMap::new();
    let mut prev_was_list = false;

    for block in blocks {
        match block.kind {
            DocxBlockKind::Table => {
                if !block.text.trim().is_empty() {
                    if prev_was_list {
                        out.push('\n');
                    }
                    out.push('\n');
                    out.push_str(block.text.trim());
                    out.push_str("\n\n");
                }
                list_counters.clear();
                prev_was_list = false;
            }
            DocxBlockKind::Paragraph => {
                let raw = block.text.trim();

                if raw.is_empty() && !block.has_drawing {
                    continue;
                }

                for id in &block.footnote_refs {
                    if let Some(t) = footnote_map.get(id) {
                        footnote_queue.push((id.clone(), t.clone()));
                    }
                }
                for id in &block.endnote_refs {
                    if let Some(t) = endnote_map.get(id) {
                        footnote_queue.push((id.clone(), t.clone()));
                    }
                }

                let text = if !block.hyperlinks.is_empty() {
                    let mut t = raw.to_string();
                    for (anchor, rid) in &block.hyperlinks {
                        if let Some(url) = rels_map.get(rid) {
                            let a = anchor.trim();
                            if !a.is_empty() {
                                t = t.replacen(a, &format!("[{}]({})", a, url), 1);
                            }
                        }
                    }
                    t
                } else {
                    raw.to_string()
                };

                let heading_level =
                    docx_heading_level(block.heading_style.as_deref(), block.outline_level);

                if let Some(level) = heading_level {
                    if prev_was_list {
                        out.push('\n');
                    }
                    list_counters.clear();
                    let hashes = "#".repeat((level as usize).min(6));
                    out.push_str(&format!("{} {}\n\n", hashes, text));
                    prev_was_list = false;
                } else if block.has_drawing {
                    if prev_was_list {
                        out.push('\n');
                    }
                    list_counters.clear();

                    let mut emitted = false;
                    if let Some(rid) = block.image_rid.as_deref() {
                        if let Some(zip_path) = image_rids_map.get(rid) {
                            if let Ok(mut entry) = archive.by_name(zip_path) {
                                let mut bytes = Vec::new();
                                if entry.read_to_end(&mut bytes).is_ok() {
                                    if let Some(hash_name) = image_hash_name(&bytes, zip_path) {
                                        if !image_out.iter().any(|(name, _)| name == &hash_name) {
                                            image_out.push((hash_name.clone(), bytes));
                                        }
                                        out.push_str(&format!("![]({})\n\n", hash_name));
                                        emitted = true;
                                    }
                                }
                            }
                        }
                    }

                    if !emitted {
                        let placeholder = image_placeholder(block.image_alt.as_deref());
                        out.push_str(&format!("{}\n\n", placeholder));
                    }
                    prev_was_list = false;
                } else if block.is_list {
                    let indent = "  ".repeat(block.list_level as usize);
                    let is_ordered = block
                        .num_id
                        .and_then(|id| num_id_map.get(&id))
                        .copied()
                        .unwrap_or_else(|| {
                            let s = block
                                .heading_style
                                .as_deref()
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            s.contains("number") && !s.contains("unnumber")
                        });

                    if is_ordered {
                        let key = (block.num_id.unwrap_or(0), block.list_level);
                        let counter = list_counters.entry(key).or_insert(0);
                        *counter += 1;
                        out.push_str(&format!("{}{}. {}\n", indent, counter, text));
                    } else {
                        out.push_str(&format!("{}- {}\n", indent, text));
                    }
                    prev_was_list = true;
                } else {
                    if prev_was_list {
                        out.push('\n');
                    }
                    list_counters.clear();
                    let style_lc = block
                        .heading_style
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if style_lc.contains("code") || text.contains("```") {
                        out.push_str(&format!("```\n{}\n```\n\n", text));
                    } else {
                        if block.page_break || block.section_break {
                            out.push_str("---\n\n");
                        }
                        out.push_str(&text);
                        out.push_str("\n\n");
                    }
                    prev_was_list = false;
                }
            }
        }
    }

    if !footnote_queue.is_empty() {
        out.push_str("---\n\n");
        for (id, text) in &footnote_queue {
            out.push_str(&format!("[^{}]: {}\n", id, text));
        }
        out.push('\n');
    }

    let mut result = String::with_capacity(out.len());
    let mut blank_count = 0u32;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }

    result.trim_end().to_string()
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────


fn read_aux(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<
    (
        HashMap<String, String>, // footnote_map
        HashMap<String, String>, // endnote_map
        HashMap<u32, bool>,      // num_id_map
        HashMap<String, String>, // rels_map
        HashMap<String, String>, // image_rids_map
        String,                  // header_text
    ),
    String,
> {
    let footnotes_xml = read_zip_entry(archive, "word/footnotes.xml", MAX_AUX_XML_BYTES)?;
    let endnotes_xml = read_zip_entry(archive, "word/endnotes.xml", MAX_AUX_XML_BYTES)?;
    let numbering_xml = read_zip_entry(archive, "word/numbering.xml", MAX_AUX_XML_BYTES)?;
    let rels_xml = read_zip_entry(archive, "word/_rels/document.xml.rels", MAX_AUX_XML_BYTES)?;

    let footnote_map = footnotes_xml.as_deref().map(|x| extract_notes_map(x, "footnote")).unwrap_or_default();
    let endnote_map = endnotes_xml.as_deref().map(|x| extract_notes_map(x, "endnote")).unwrap_or_default();
    let num_id_map = numbering_xml.as_deref().map(parse_numbering_xml).unwrap_or_default();
    let (rels_map, header_paths) = rels_xml.as_deref().map(parse_rels_xml).unwrap_or_default();
    let image_rids_map = rels_xml.as_deref().map(parse_rels_xml_images).unwrap_or_default();

    let mut parts: Vec<String> = Vec::new();
    for path in &header_paths {
        if let Ok(Some(xml)) = read_zip_entry(archive, path, MAX_AUX_XML_BYTES) {
            let t = extract_header_text(&xml);
            if !t.is_empty() && !parts.contains(&t) {
                parts.push(t);
            }
        }
    }
    let header_text = parts.join(" | ");
    Ok((footnote_map, endnote_map, num_id_map, rels_map, image_rids_map, header_text))
}

pub(super) fn to_markdown(bytes: &[u8]) -> Result<String, String> {
    let blocks = parse_docx_blocks(bytes)?;
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).map_err(|e| format!("Not a valid DOCX zip: {e}"))?;
    let (footnote_map, endnote_map, num_id_map, rels_map, _img, header_text) = read_aux(&mut archive)?;
    let body = blocks_to_markdown(&blocks, &footnote_map, &endnote_map, &num_id_map, &rels_map);
    Ok(if header_text.is_empty() {
        body
    } else {
        format!("> {}\n\n{}", header_text, body)
    })
}

pub(super) fn to_markdown_with_images(bytes: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let blocks = parse_docx_blocks(bytes)?;
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).map_err(|e| format!("Not a valid DOCX zip: {e}"))?;
    let (footnote_map, endnote_map, num_id_map, rels_map, image_rids_map, header_text) = read_aux(&mut archive)?;
    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    let body = blocks_to_markdown_with_images(
        &blocks, &footnote_map, &endnote_map, &num_id_map, &rels_map, &image_rids_map,
        &mut archive, &mut image_out,
    );
    let md = if header_text.is_empty() {
        body
    } else {
        format!("> {}\n\n{}", header_text, body)
    };
    Ok((md, image_out))
}
