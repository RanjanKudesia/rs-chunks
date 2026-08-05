use crate::entities::read_event_folding_entities;
use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

use super::common::{parse_presentation_sections, read_zip_entry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    ListItem,
    Table,
    Image,
}

#[derive(Debug, Clone)]
struct SlideBlock {
    kind: BlockKind,
    /// Plain or inline-formatted text (for Paragraph, ListItem, Image).
    text: String,
    /// Indent level 0-based (for ListItem).
    level: u8,
    /// True = ordered/numbered list, false = bullet (for ListItem).
    is_numbered: bool,
    /// Table rows: outer = rows, inner = cells (for Table).
    table_rows: Vec<Vec<String>>,
    /// Whether to render the first row as a header separator (for Table).
    table_has_header: bool,
    /// Relationship id from `<a:blip r:embed="rIdN"/>` for image blocks.
    image_rid: Option<String>,
}

impl SlideBlock {
    fn paragraph(text: String) -> Self {
        Self {
            kind: BlockKind::Paragraph,
            text,
            level: 0,
            is_numbered: false,
            table_rows: Vec::new(),
            table_has_header: false,
            image_rid: None,
        }
    }

    fn list_item(text: String, level: u8, is_numbered: bool) -> Self {
        Self {
            kind: BlockKind::ListItem,
            text,
            level,
            is_numbered,
            table_rows: Vec::new(),
            table_has_header: false,
            image_rid: None,
        }
    }

    fn table(rows: Vec<Vec<String>>, has_header: bool) -> Self {
        Self {
            kind: BlockKind::Table,
            text: String::new(),
            level: 0,
            is_numbered: false,
            table_rows: rows,
            table_has_header: has_header,
            image_rid: None,
        }
    }

    fn image(alt: Option<String>, rid: Option<String>) -> Self {
        let text = match alt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(a) => format!("[Image: {a}]"),
            None => "[Image]".to_string(),
        };
        Self {
            kind: BlockKind::Image,
            text,
            level: 0,
            is_numbered: false,
            table_rows: Vec::new(),
            table_has_header: false,
            image_rid: rid,
        }
    }
}

#[derive(Debug, Default)]
struct SlideMarkdownContent {
    pub title: Option<String>,
    pub blocks: Vec<SlideBlock>,
    pub notes: Option<String>,
    /// `r:dm` ids of SmartArt diagrams; their text lives in `ppt/diagrams/`.
    pub diagram_rids: Vec<String>,
    /// `r:id` ids of charts; their data lives in `ppt/charts/`.
    pub chart_rids: Vec<String>,
}

/// Append a slide's SmartArt text as paragraph blocks. Shared by both markdown
/// entry points so `get_markdown` and `get_markdown_with_images` agree.
fn append_diagram_blocks(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
    slide: &mut SlideMarkdownContent,
) {
    let parts = super::diagram::resolve_diagram_parts(archive, slide_name, &slide.diagram_rids);
    for part in parts {
        if let Ok(bytes) = read_zip_entry(archive, &part) {
            for text in super::diagram::parse_diagram_xml(&bytes) {
                slide.blocks.push(SlideBlock::paragraph(text));
            }
        }
    }
}

/// Append a slide's chart data as a real markdown table. The block renderer
/// already knows how to draw one, so a chart costs no new rendering code.
fn append_chart_blocks(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
    slide: &mut SlideMarkdownContent,
) {
    let parts = super::chart::resolve_chart_parts(archive, slide_name, &slide.chart_rids);
    for part in parts {
        if let Ok(bytes) = read_zip_entry(archive, &part) {
            let rows = super::chart::parse_chart_xml(&bytes);
            if !rows.is_empty() {
                slide.blocks.push(SlideBlock::paragraph("Chart".to_string()));
                slide.blocks.push(SlideBlock::table(rows, true));
            }
        }
    }
}

fn push_text(dst: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !dst.is_empty() {
        dst.push(' ');
    }
    dst.push_str(trimmed);
}

fn attr_local_name(key: &[u8]) -> &[u8] {
    key.rsplit(|b| *b == b':').next().unwrap_or(key)
}

/// Resolve XML entities in an attribute value, falling back to the raw bytes if
/// the value references an entity quick-xml cannot expand.
fn decode_attr(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    match attr.unescape_value() {
        Ok(v) => v.into_owned(),
        Err(_) => String::from_utf8_lossy(attr.value.as_ref()).into_owned(),
    }
}

/// Squash any run of whitespace (including the newlines `&#xA;` decodes to)
/// into single spaces, so the text can be rendered on one line.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_slide_for_markdown(
    xml_bytes: &[u8],
    rels: &HashMap<String, String>,
) -> Result<SlideMarkdownContent, String> {
    let mut reader = XmlReader::from_reader(Cursor::new(xml_bytes));
    let mut buf = Vec::new();

    // Shape tracking
    let mut sp_depth: i32 = 0;
    let mut sp_is_title = false;
    let mut sp_ph_checked = false;
    let mut in_pic = false;
    // Slide-background fill: <p:bg><p:bgPr><a:blipFill><a:blip r:embed>. It is
    // never inside a <p:pic>, so the `in_pic` gate below skipped it and a
    // background-only slide rendered with no image at all — while the chunk
    // path now extracts it, leaving the two disagreeing (TECH_DEBT #17).
    let mut in_bg = false;
    let mut bg_rid: Option<String> = None;
    let mut pic_alt: Option<String> = None;
    let mut pic_rid: Option<String> = None;

    // Text body / paragraph tracking (inside sp)
    let mut in_txbody = false;
    let mut in_para = false;
    let mut para_level: u8 = 0;
    let mut para_has_bullet = false;
    let mut para_is_numbered = false;
    let mut para_explicit_bu_none = false;
    let mut para_in_ppr = false;
    let mut para_text = String::new();

    // Run tracking (inside a:r)
    let mut in_run = false;
    let mut cur_bold = false;
    let mut cur_italic = false;
    let mut cur_run_text = String::new();
    let mut cur_hlink_rid = String::new(); // r:id from <a:hlinkClick> in current rPr

    // <a:t> tracking
    let mut in_t = false;

    // Table tracking
    let mut in_tbl = false;
    let mut tbl_has_header = false;
    let mut in_tbl_ppr = false;
    let mut tbl_rows: Vec<Vec<String>> = Vec::new();
    let mut tbl_current_row: Vec<String> = Vec::new();
    let mut in_tc = false;
    let mut tc_text = String::new();
    let mut in_tc_body = false;
    let mut in_tc_para = false;
    let mut tc_para_text = String::new();
    let mut in_tc_t = false;

    // Shape paragraphs accumulation
    let mut shape_paragraphs: Vec<SlideBlock> = Vec::new();

    let mut slide = SlideMarkdownContent::default();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(XmlEvent::Start(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);

                match local {
                    // Group shapes are traversed transparently; the tag itself
                    // needs no state — shapes inside are handled as normal.
                    b"grpSp" => {}
                    b"sp" => {
                        sp_depth += 1;
                        if sp_depth == 1 {
                            sp_is_title = false;
                            sp_ph_checked = false;
                            shape_paragraphs.clear();
                        }
                    }
                    b"ph" if sp_depth > 0 && !sp_ph_checked => {
                        sp_ph_checked = true;
                        let mut ph_type: Option<String> = None;
                        let mut ph_idx: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            let val = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            match key_local {
                                b"type" => ph_type = Some(val),
                                b"idx" => ph_idx = Some(val),
                                _ => {}
                            }
                        }
                        if let Some(t) = ph_type {
                            let t = t.to_ascii_lowercase();
                            sp_is_title = matches!(t.as_str(), "title" | "ctrtitle" | "subtitle");
                        } else if ph_idx.as_deref() == Some("0") {
                            sp_is_title = true;
                        }
                    }
                    b"txBody" if in_tc => {
                        in_tc_body = true;
                    }
                    b"txBody" if sp_depth > 0 && !in_tbl => {
                        in_txbody = true;
                    }
                    b"p" if in_txbody => {
                        in_para = true;
                        para_text.clear();
                        para_level = 0;
                        para_has_bullet = false;
                        para_is_numbered = false;
                        para_explicit_bu_none = false;
                        para_in_ppr = false;
                        cur_hlink_rid.clear();
                    }
                    b"pPr" if in_para => {
                        para_in_ppr = true;
                        for attr in e.attributes().flatten() {
                            if attr_local_name(attr.key.as_ref()) == b"lvl" {
                                let v = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .parse::<u8>()
                                    .unwrap_or(0);
                                para_level = v;
                                break;
                            }
                        }
                    }
                    b"buChar" if para_in_ppr => {
                        para_has_bullet = true;
                        para_is_numbered = false;
                    }
                    b"buAutoNum" if para_in_ppr => {
                        para_has_bullet = true;
                        para_is_numbered = true;
                    }
                    b"buNone" if para_in_ppr => {
                        para_has_bullet = false;
                        para_is_numbered = false;
                        para_explicit_bu_none = true;
                    }
                    b"r" if in_para => {
                        in_run = true;
                        cur_bold = false;
                        cur_italic = false;
                        cur_run_text.clear();
                    }
                    b"rPr" if in_run => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            let val =
                                String::from_utf8_lossy(attr.value.as_ref()).to_ascii_lowercase();
                            match key_local {
                                b"b" => cur_bold = val == "1" || val == "true",
                                b"i" => cur_italic = val == "1" || val == "true",
                                _ => {}
                            }
                        }
                    }
                    b"hlinkClick" if in_run => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            if key_local == b"id" {
                                cur_hlink_rid = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                break;
                            }
                        }
                    }
                    b"t" if in_para && !in_tc_para => {
                        in_t = true;
                    }
                    b"pic" => {
                        in_pic = true;
                        pic_alt = None;
                        pic_rid = None;
                    }
                    b"bg" => {
                        in_bg = true;
                        bg_rid = None;
                    }
                    b"cNvPr" if in_pic => {
                        let mut descr: Option<String> = None;
                        let mut name: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            // descr/name are human-authored, so they carry real
                            // entities — a raw byte read leaks "&#xA;" into the
                            // alt text. Decode, then flatten the newlines that
                            // decoding reveals: this renders inline.
                            let val = collapse_ws(&decode_attr(&attr));
                            if val.is_empty() {
                                continue;
                            }
                            match key_local {
                                b"descr" => descr = Some(val),
                                b"name" => name = Some(val),
                                _ => {}
                            }
                        }
                        if let Some(d) = descr {
                            pic_alt = Some(d);
                        } else if let Some(n) = name {
                            let lower = n.to_ascii_lowercase();
                            let generic = lower.starts_with("picture ")
                                || lower.starts_with("image ")
                                || lower.starts_with("graphic ")
                                || lower.starts_with("chart ");
                            if !generic {
                                pic_alt = Some(n);
                            }
                        }
                    }
                    b"blip" if in_pic => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            if key_local == b"embed" {
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
                    b"blip" if in_bg => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            if key_local == b"embed" {
                                let rid = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                if !rid.is_empty() {
                                    bg_rid = Some(rid);
                                }
                                break;
                            }
                        }
                    }
                    b"tbl" => {
                        in_tbl = true;
                        tbl_has_header = false;
                        tbl_rows.clear();
                    }
                    b"tblPr" if in_tbl => {
                        in_tbl_ppr = true;
                        for attr in e.attributes().flatten() {
                            if attr_local_name(attr.key.as_ref()) == b"firstRow" {
                                let v = String::from_utf8_lossy(attr.value.as_ref());
                                tbl_has_header = v.trim() == "1";
                                break;
                            }
                        }
                    }
                    b"tr" if in_tbl => {
                        tbl_current_row.clear();
                    }
                    b"tc" if in_tbl => {
                        in_tc = true;
                        tc_text.clear();
                    }
                    b"p" if in_tc_body => {
                        in_tc_para = true;
                        tc_para_text.clear();
                    }
                    b"t" if in_tc_para => {
                        in_tc_t = true;
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Empty(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);

                match local {
                    // SmartArt: the slide only points at the part holding the text.
                    // `<c:chart r:id=…/>` — the pointer to the chart part.
                    b"chart" => {
                        for attr in e.attributes().flatten() {
                            if attr_local_name(attr.key.as_ref()) == b"id" {
                                let v = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                if !v.is_empty() {
                                    slide.chart_rids.push(v);
                                }
                                break;
                            }
                        }
                    }
                    b"relIds" => {
                        for attr in e.attributes().flatten() {
                            if attr_local_name(attr.key.as_ref()) == b"dm" {
                                let v = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                if !v.is_empty() {
                                    slide.diagram_rids.push(v);
                                }
                                break;
                            }
                        }
                    }
                    b"pPr" if in_para => {
                        for attr in e.attributes().flatten() {
                            if attr_local_name(attr.key.as_ref()) == b"lvl" {
                                let v = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .parse::<u8>()
                                    .unwrap_or(0);
                                para_level = v;
                                break;
                            }
                        }
                    }
                    b"hlinkClick" if in_run => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            if key_local == b"id" {
                                cur_hlink_rid = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                break;
                            }
                        }
                    }
                    b"ph" if sp_depth > 0 && !sp_ph_checked => {
                        sp_ph_checked = true;
                        let mut ph_type: Option<String> = None;
                        let mut ph_idx: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            let val = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            match key_local {
                                b"type" => ph_type = Some(val),
                                b"idx" => ph_idx = Some(val),
                                _ => {}
                            }
                        }
                        if let Some(t) = ph_type {
                            let t = t.to_ascii_lowercase();
                            sp_is_title = matches!(t.as_str(), "title" | "ctrtitle" | "subtitle");
                        } else if ph_idx.as_deref() == Some("0") {
                            sp_is_title = true;
                        }
                    }
                    b"buChar" if para_in_ppr => {
                        para_has_bullet = true;
                        para_is_numbered = false;
                    }
                    b"buAutoNum" if para_in_ppr => {
                        para_has_bullet = true;
                        para_is_numbered = true;
                    }
                    b"buNone" if para_in_ppr => {
                        para_has_bullet = false;
                        para_is_numbered = false;
                        para_explicit_bu_none = true;
                    }
                    b"rPr" if in_run => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            let val =
                                String::from_utf8_lossy(attr.value.as_ref()).to_ascii_lowercase();
                            match key_local {
                                b"b" => cur_bold = val == "1" || val == "true",
                                b"i" => cur_italic = val == "1" || val == "true",
                                _ => {}
                            }
                        }
                    }
                    b"pic" => {
                        in_pic = false;
                        slide.blocks.push(SlideBlock::image(None, None));
                    }
                    b"cNvPr" if in_pic => {
                        let mut descr: Option<String> = None;
                        let mut name: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            // descr/name are human-authored, so they carry real
                            // entities — a raw byte read leaks "&#xA;" into the
                            // alt text. Decode, then flatten the newlines that
                            // decoding reveals: this renders inline.
                            let val = collapse_ws(&decode_attr(&attr));
                            if val.is_empty() {
                                continue;
                            }
                            match key_local {
                                b"descr" => descr = Some(val),
                                b"name" => name = Some(val),
                                _ => {}
                            }
                        }
                        if let Some(d) = descr {
                            pic_alt = Some(d);
                        } else if let Some(n) = name {
                            let lower = n.to_ascii_lowercase();
                            let generic = lower.starts_with("picture ")
                                || lower.starts_with("image ")
                                || lower.starts_with("graphic ")
                                || lower.starts_with("chart ");
                            if !generic {
                                pic_alt = Some(n);
                            }
                        }
                    }
                    b"blip" if in_pic => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            if key_local == b"embed" {
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
                    b"blip" if in_bg => {
                        for attr in e.attributes().flatten() {
                            let key_local = attr_local_name(attr.key.as_ref());
                            if key_local == b"embed" {
                                let rid = String::from_utf8_lossy(attr.value.as_ref())
                                    .trim()
                                    .to_string();
                                if !rid.is_empty() {
                                    bg_rid = Some(rid);
                                }
                                break;
                            }
                        }
                    }
                    b"tblPr" if in_tbl => {
                        for attr in e.attributes().flatten() {
                            if attr_local_name(attr.key.as_ref()) == b"firstRow" {
                                let v = String::from_utf8_lossy(attr.value.as_ref());
                                tbl_has_header = v.trim() == "1";
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Text(ref e)) => {
                if in_t && in_para && !in_tc_para {
                    let txt = e.decode().unwrap_or_default().to_string();
                    if in_run {
                        push_text(&mut cur_run_text, &txt);
                    } else {
                        push_text(&mut para_text, &txt);
                    }
                }
                if in_tc_t {
                    let txt = e.decode().unwrap_or_default().to_string();
                    push_text(&mut tc_para_text, &txt);
                }
            }
            Ok(XmlEvent::CData(ref e)) => {
                if in_t && in_para && !in_tc_para {
                    let txt = String::from_utf8_lossy(e.as_ref()).to_string();
                    if in_run {
                        push_text(&mut cur_run_text, &txt);
                    } else {
                        push_text(&mut para_text, &txt);
                    }
                }
                if in_tc_t {
                    let txt = String::from_utf8_lossy(e.as_ref()).to_string();
                    push_text(&mut tc_para_text, &txt);
                }
            }
            Ok(XmlEvent::End(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);

                match local {
                    b"grpSp" => {}
                    b"r" if in_para => {
                        if !cur_run_text.is_empty() {
                            let formatted = match (cur_bold, cur_italic) {
                                (true, true) => format!("***{}***", cur_run_text),
                                (true, false) => format!("**{}**", cur_run_text),
                                (false, true) => format!("*{}*", cur_run_text),
                                _ => cur_run_text.clone(),
                            };
                            let final_text = if !cur_hlink_rid.is_empty() {
                                if let Some(url) = rels.get(&cur_hlink_rid) {
                                    format!("[{}]({})", formatted, url)
                                } else {
                                    formatted
                                }
                            } else {
                                formatted
                            };
                            push_text(&mut para_text, &final_text);
                        }
                        in_run = false;
                        cur_bold = false;
                        cur_italic = false;
                        cur_run_text.clear();
                        cur_hlink_rid.clear();
                    }
                    b"t" if in_t => {
                        in_t = false;
                    }
                    b"t" if in_tc_t => {
                        in_tc_t = false;
                    }
                    b"pPr" if para_in_ppr => {
                        para_in_ppr = false;
                    }
                    b"p" if in_para => {
                        in_para = false;
                        let trimmed = para_text.trim().to_string();
                        if !trimmed.is_empty() {
                            let inferred_bullet = !sp_is_title && !para_explicit_bu_none;
                            if para_has_bullet || (inferred_bullet && !para_is_numbered) {
                                shape_paragraphs.push(SlideBlock::list_item(
                                    trimmed,
                                    para_level,
                                    para_is_numbered,
                                ));
                            } else {
                                shape_paragraphs.push(SlideBlock::paragraph(trimmed));
                            }
                        }
                        para_text.clear();
                    }
                    b"txBody" if in_txbody && !in_tbl => {
                        in_txbody = false;
                    }
                    b"sp" if sp_depth > 0 => {
                        sp_depth -= 1;
                        if sp_depth == 0 {
                            if sp_is_title && slide.title.is_none() {
                                let title_parts: Vec<String> = shape_paragraphs
                                    .iter()
                                    .filter_map(|b| {
                                        if matches!(
                                            b.kind,
                                            BlockKind::Paragraph | BlockKind::ListItem
                                        ) {
                                            Some(b.text.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                if !title_parts.is_empty() {
                                    slide.title = Some(title_parts.join(" "));
                                }
                            } else {
                                slide.blocks.extend(shape_paragraphs.drain(..));
                            }
                            shape_paragraphs.clear();
                            sp_is_title = false;
                            sp_ph_checked = false;
                        }
                    }
                    b"pic" if in_pic => {
                        in_pic = false;
                        slide
                            .blocks
                            .push(SlideBlock::image(pic_alt.take(), pic_rid.take()));
                    }
                    b"bg" if in_bg => {
                        in_bg = false;
                        if let Some(rid) = bg_rid.take() {
                            slide.blocks.push(SlideBlock::image(None, Some(rid)));
                        }
                    }
                    b"p" if in_tc_para => {
                        in_tc_para = false;
                        let t = tc_para_text.trim().to_string();
                        if !t.is_empty() {
                            if !tc_text.is_empty() {
                                tc_text.push(' ');
                            }
                            tc_text.push_str(&t);
                        }
                        tc_para_text.clear();
                    }
                    b"txBody" if in_tc_body => {
                        in_tc_body = false;
                    }
                    b"tc" if in_tc => {
                        in_tc = false;
                        tbl_current_row.push(tc_text.trim().to_string());
                        tc_text.clear();
                    }
                    b"tr" if in_tbl => {
                        if !tbl_current_row.is_empty() {
                            tbl_rows.push(std::mem::take(&mut tbl_current_row));
                        }
                    }
                    b"tbl" if in_tbl => {
                        in_tbl = false;
                        if !tbl_rows.is_empty() {
                            let has_hdr = tbl_has_header || tbl_rows.len() > 1;
                            slide
                                .blocks
                                .push(SlideBlock::table(std::mem::take(&mut tbl_rows), has_hdr));
                        }
                    }
                    b"tblPr" if in_tbl_ppr => {
                        in_tbl_ppr = false;
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(e) => return Err(format!("PPTX slide XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(slide)
}

/// Extract the presentation title from docProps/core.xml (Dublin Core metadata).
fn extract_presentation_title(archive: &mut ZipArchive<Cursor<Vec<u8>>>) -> Option<String> {
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
fn parse_slide_rels(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    slide_name: &str,
) -> HashMap<String, String> {
    parse_slide_rels_with_images(archive, slide_name).0
}

fn parse_slide_rels_with_images(
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

fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    let path = zip_path.to_ascii_lowercase();
    crate::image_naming::name_for_path(bytes, &path)
}

fn resolve_relative_path(base_dir: &str, relative: &str) -> String {
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

fn extract_notes_text(
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
            Some(resolve_relative_path(dir, &rest[..end]))
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

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', " ")
}

fn render_table_md(rows: &[Vec<String>], has_header: bool) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for col in 0..max_cols {
            let cell = row.get(col).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&escape_pipe(cell));
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 && has_header && rows.len() > 1 {
            out.push('|');
            for _ in 0..max_cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out.trim_end_matches('\n').to_string()
}

fn slide_to_markdown(slide_num: usize, slide: &SlideMarkdownContent) -> String {
    let mut out = String::new();

    // Heading
    match &slide.title {
        Some(t) => out.push_str(&format!("## Slide {}: {}\n\n", slide_num, t.trim())),
        None => out.push_str(&format!("## Slide {}\n\n", slide_num)),
    }

    // Numbered list counters per level
    let mut num_counters: HashMap<u8, usize> = HashMap::new();
    let mut prev_was_list = false;

    for block in &slide.blocks {
        match block.kind {
            BlockKind::Paragraph => {
                if prev_was_list {
                    out.push('\n');
                }
                num_counters.clear();
                if !block.text.trim().is_empty() {
                    out.push_str(block.text.trim());
                    out.push_str("\n\n");
                }
                prev_was_list = false;
            }
            BlockKind::ListItem => {
                let indent = "  ".repeat(block.level as usize);
                if block.is_numbered {
                    let counter = num_counters.entry(block.level).or_insert(0);
                    *counter += 1;
                    out.push_str(&format!("{}{}. {}\n", indent, counter, block.text));
                } else {
                    // Reset numbered counters when switching to bullet at same level
                    num_counters.remove(&block.level);
                    out.push_str(&format!("{}- {}\n", indent, block.text));
                }
                prev_was_list = true;
            }
            BlockKind::Table => {
                if prev_was_list {
                    out.push('\n');
                }
                num_counters.clear();
                let rendered = render_table_md(&block.table_rows, block.table_has_header);
                if !rendered.is_empty() {
                    out.push_str(&rendered);
                    out.push_str("\n\n");
                }
                prev_was_list = false;
            }
            BlockKind::Image => {
                if prev_was_list {
                    out.push('\n');
                }
                num_counters.clear();
                out.push_str(&block.text);
                out.push_str("\n\n");
                prev_was_list = false;
            }
        }
    }

    // Speaker notes
    if let Some(ref notes) = slide.notes {
        let n = notes.trim();
        if !n.is_empty() {
            if prev_was_list {
                out.push('\n');
            }
            out.push_str(&format!("> **Notes:** {}\n\n", n.replace('\n', " ")));
        }
    }

    out.trim_end().to_string()
}

fn slide_to_markdown_with_images(
    slide_num: usize,
    slide: &SlideMarkdownContent,
    image_rids: &HashMap<String, String>,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    image_out: &mut Vec<(String, Vec<u8>)>,
) -> String {
    let mut out = String::new();

    match &slide.title {
        Some(t) => out.push_str(&format!("## Slide {}: {}\n\n", slide_num, t.trim())),
        None => out.push_str(&format!("## Slide {}\n\n", slide_num)),
    }

    let mut num_counters: HashMap<u8, usize> = HashMap::new();
    let mut prev_was_list = false;

    for block in &slide.blocks {
        match block.kind {
            BlockKind::Paragraph => {
                if prev_was_list {
                    out.push('\n');
                }
                num_counters.clear();
                if !block.text.trim().is_empty() {
                    out.push_str(block.text.trim());
                    out.push_str("\n\n");
                }
                prev_was_list = false;
            }
            BlockKind::ListItem => {
                let indent = "  ".repeat(block.level as usize);
                if block.is_numbered {
                    let counter = num_counters.entry(block.level).or_insert(0);
                    *counter += 1;
                    out.push_str(&format!("{}{}. {}\n", indent, counter, block.text));
                } else {
                    num_counters.remove(&block.level);
                    out.push_str(&format!("{}- {}\n", indent, block.text));
                }
                prev_was_list = true;
            }
            BlockKind::Table => {
                if prev_was_list {
                    out.push('\n');
                }
                num_counters.clear();
                let rendered = render_table_md(&block.table_rows, block.table_has_header);
                if !rendered.is_empty() {
                    out.push_str(&rendered);
                    out.push_str("\n\n");
                }
                prev_was_list = false;
            }
            BlockKind::Image => {
                if prev_was_list {
                    out.push('\n');
                }
                num_counters.clear();

                let mut emitted = false;
                if let Some(rid) = block.image_rid.as_deref() {
                    if let Some(zip_path) = image_rids.get(rid) {
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
                    out.push_str(&block.text);
                    out.push_str("\n\n");
                }
                prev_was_list = false;
            }
        }
    }

    if let Some(ref notes) = slide.notes {
        let n = notes.trim();
        if !n.is_empty() {
            if prev_was_list {
                out.push('\n');
            }
            out.push_str(&format!("> **Notes:** {}\n\n", n.replace('\n', " ")));
        }
    }

    out.trim_end().to_string()
}

fn presentation_to_markdown(
    pres_title: Option<String>,
    slides: Vec<(usize, SlideMarkdownContent)>,
    sections: Vec<(String, Vec<usize>)>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(ref t) = pres_title {
        if !t.trim().is_empty() {
            parts.push(format!("# {}", t.trim()));
        }
    }

    // Build a map: slide_number -> section_name for the first slide in each section
    let mut section_starts: HashMap<usize, String> = HashMap::new();
    for (section_name, slide_nums) in &sections {
        if let Some(&first) = slide_nums.first() {
            section_starts.insert(first, section_name.clone());
        }
    }

    for (slide_num, slide) in &slides {
        if let Some(section_name) = section_starts.get(slide_num) {
            parts.push(format!("# {}", section_name.trim()));
        }
        let slide_md = slide_to_markdown(*slide_num, slide);
        if !slide_md.trim().is_empty() {
            parts.push(slide_md);
        }
    }

    parts.join("\n\n---\n\n").trim().to_string()
}


pub(super) fn to_markdown(bytes: &[u8]) -> Result<String, String> {
    use super::common::{collect_slide_names, open_pptx};
    let mut archive = open_pptx(bytes).map_err(|e| format!("Not a valid PPTX zip: {e}"))?;

    let pres_title = extract_presentation_title(&mut archive);
    let sections = parse_presentation_sections(&mut archive).unwrap_or_default();
    let slide_names = collect_slide_names(&archive);

    let mut slides: Vec<(usize, SlideMarkdownContent)> = Vec::new();
    for (slide_num, slide_name) in &slide_names {
        let xml_bytes = read_zip_entry(&mut archive, slide_name)?;
        let slide_rels = parse_slide_rels(&mut archive, slide_name);
        let mut slide = parse_slide_for_markdown(&xml_bytes, &slide_rels)?;
        append_diagram_blocks(&mut archive, slide_name, &mut slide);
        append_chart_blocks(&mut archive, slide_name, &mut slide);
        slide.notes = extract_notes_text(&mut archive, slide_name);
        slides.push((*slide_num, slide));
    }
    Ok(presentation_to_markdown(pres_title, slides, sections))
}

pub(super) fn to_markdown_with_images(bytes: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    use super::common::{collect_slide_names, open_pptx};
    let mut archive = open_pptx(bytes).map_err(|e| format!("Not a valid PPTX zip: {e}"))?;
    let pres_title = extract_presentation_title(&mut archive);
    let sections = parse_presentation_sections(&mut archive).unwrap_or_default();
    let slide_names = collect_slide_names(&archive);

    let mut slides: Vec<(usize, SlideMarkdownContent)> = Vec::new();
    let mut slide_image_rids: std::collections::HashMap<usize, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    for (slide_num, slide_name) in &slide_names {
        let xml_bytes = read_zip_entry(&mut archive, slide_name)?;
        let (slide_rels, image_rids) = parse_slide_rels_with_images(&mut archive, slide_name);
        let mut slide = parse_slide_for_markdown(&xml_bytes, &slide_rels)?;
        append_diagram_blocks(&mut archive, slide_name, &mut slide);
        append_chart_blocks(&mut archive, slide_name, &mut slide);
        slide.notes = extract_notes_text(&mut archive, slide_name);
        slides.push((*slide_num, slide));
        slide_image_rids.insert(*slide_num, image_rids);
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(ref t) = pres_title {
        if !t.trim().is_empty() {
            parts.push(format!("# {}", t.trim()));
        }
    }
    let mut section_starts: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for (section_name, slide_nums) in &sections {
        if let Some(&first) = slide_nums.first() {
            section_starts.insert(first, section_name.clone());
        }
    }

    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    for (slide_num, slide) in &slides {
        if let Some(section_name) = section_starts.get(slide_num) {
            parts.push(format!("# {}", section_name.trim()));
        }
        let image_rids = slide_image_rids.get(slide_num).cloned().unwrap_or_default();
        let slide_md = slide_to_markdown_with_images(*slide_num, slide, &image_rids, &mut archive, &mut image_out);
        if !slide_md.trim().is_empty() {
            parts.push(slide_md);
        }
    }
    Ok((parts.join("\n\n---\n\n").trim().to_string(), image_out))
}
