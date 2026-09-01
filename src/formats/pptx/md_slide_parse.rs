//! Slide XML -> markdown block-stream parser.

use crate::entities::read_event_folding_entities;

use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;

use super::md_blocks::{
    attr_local_name, collapse_ws, decode_attr, push_text, BlockKind, SlideBlock,
    SlideMarkdownContent,
};

use std::collections::HashMap;
use std::io::Cursor;

/// Record a `<c:chart r:id>` or `<dgm:relIds r:dm>` pointer.
///
/// Called from BOTH the `Start` and `Empty` branches. These elements are almost
/// always self-closing, but a producer may write them in start/end form — and
/// the arms lived only under `Empty`, so such a slide lost its chart and
/// SmartArt text through `get_markdown` while `get_chunks`, which handles both
/// forms, kept them.
fn record_graphic_pointer(
    local: &[u8],
    e: &quick_xml::events::BytesStart,
    slide: &mut SlideMarkdownContent,
) {
    let want: &[u8] = match local {
        b"chart" => b"id",
        b"relIds" => b"dm",
        _ => return,
    };
    for attr in e.attributes().flatten() {
        if attr_local_name(attr.key.as_ref()) == want {
            let v = String::from_utf8_lossy(attr.value.as_ref())
                .trim()
                .to_string();
            if !v.is_empty() {
                if local == b"chart" {
                    slide.chart_rids.push(v);
                } else {
                    slide.diagram_rids.push(v);
                }
            }
            break;
        }
    }
}

pub(super) fn parse_slide_for_markdown(
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
    // Inside an <a:fld> carrying slide chrome (slide number, date/time,
    // footer): the cached value is stale by definition and is not content.
    let mut in_chrome_fld = false;
    // Whole `<a:t>` text, accumulated verbatim and trimmed ONCE at `</a:t>`.
    let mut t_buf = String::new();
    // True until the first text event of the current <a:t> has been appended.
    // Text inside ONE element must concatenate verbatim — an entity reference
    // splits it into several events, and space-joining them produced `AT & T`
    // from `AT&amp;T` (TECH_DEBT L6). Spacing belongs between elements, not
    // inside one.

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
    let mut tc_t_buf = String::new();

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
                    // Charts and SmartArt are usually self-closing, but a
                    // producer may write them in start/end form — and these
                    // arms existed only under `Empty`, so such a slide silently
                    // lost its chart and SmartArt text here while `get_chunks`
                    // kept them.
                    b"chart" | b"relIds" => record_graphic_pointer(local, e, &mut slide),
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
                    b"fld" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with(b"type") {
                                let v = String::from_utf8_lossy(a.value.as_ref())
                                    .to_ascii_lowercase();
                                if v == "slidenum" || v == "ftr" || v.starts_with("datetime") {
                                    in_chrome_fld = true;
                                }
                            }
                        }
                    }
                    b"t" if in_para && !in_tc_para && !in_chrome_fld => {
                        in_t = true;
                        t_buf.clear();
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
                    b"t" if in_tc_para && !in_chrome_fld => {
                        in_tc_t = true;
                        tc_t_buf.clear();
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Empty(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);

                match local {
                    // SmartArt and charts: the slide only points at the part
                    // holding the text. Handled from BOTH branches — see
                    // `record_graphic_pointer`.
                    b"chart" | b"relIds" => record_graphic_pointer(local, e, &mut slide),
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
                // `is_entity` is set by read_event_folding_entities! when this
                // event came from a reference. A reference splits one element's
                // text into several events, so space-joining them turns
                // `AT&amp;T` into `AT & T` — append verbatim instead (L6).
                // Accumulate the whole `<a:t>` and trim once at `</a:t>`, the
                // way `slide_xml` already does. Trimming the FIRST segment and
                // appending the rest verbatim ate the space before an entity:
                // `O'Reilly &amp; Associates` came out `O'Reilly& Associates`
                // in markdown while `get_chunks` had it right. One deck, two
                // readings.
                if in_t && in_para && !in_tc_para {
                    t_buf.push_str(e.decode().unwrap_or_default().as_ref());
                }
                if in_tc_t {
                    tc_t_buf.push_str(e.decode().unwrap_or_default().as_ref());
                }
            }
            Ok(XmlEvent::CData(ref e)) => {
                if in_t && in_para && !in_tc_para {
                    t_buf.push_str(&String::from_utf8_lossy(e.as_ref()));
                }
                if in_tc_t {
                    tc_t_buf.push_str(&String::from_utf8_lossy(e.as_ref()));
                }
            }
            Ok(XmlEvent::End(ref e)) => {
                let ename = e.name();
                let ebytes = ename.as_ref();
                let local: &[u8] = ebytes.rsplit(|b| *b == b':').next().unwrap_or(ebytes);
                if local == b"fld" {
                    in_chrome_fld = false;
                }

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
                        let txt = std::mem::take(&mut t_buf);
                        let dst = if in_run {
                            &mut cur_run_text
                        } else {
                            &mut para_text
                        };
                        push_text(dst, txt.trim());
                    }
                    b"t" if in_tc_t => {
                        in_tc_t = false;
                        let txt = std::mem::take(&mut tc_t_buf);
                        push_text(&mut tc_para_text, txt.trim());
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
                                slide.blocks.append(&mut shape_paragraphs);
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
                    b"tr" if in_tbl && !tbl_current_row.is_empty() => {
                        tbl_rows.push(std::mem::take(&mut tbl_current_row));
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

#[cfg(test)]
mod graphic_pointer_tests {
    /// `<c:chart>` and `<dgm:relIds>` are almost always self-closing, but a
    /// producer may write them in start/end form — and the arms existed only
    /// under `Empty`, so such a slide lost its chart and SmartArt text through
    /// `get_markdown` while `get_chunks` kept them. No fixture uses the
    /// start/end form, so this needs a synthetic input.
    #[test]
    fn a_start_form_chart_pointer_is_recorded() {
        let xml = br#"<?xml version="1.0"?>
<p:sld xmlns:p="p" xmlns:a="a" xmlns:c="c" xmlns:dgm="dgm" xmlns:r="r">
 <p:cSld><p:spTree>
  <p:graphicFrame><a:graphic><a:graphicData>
    <c:chart r:id="rId9"></c:chart>
  </a:graphicData></a:graphic></p:graphicFrame>
  <p:graphicFrame><a:graphic><a:graphicData>
    <dgm:relIds r:dm="rId12"></dgm:relIds>
  </a:graphicData></a:graphic></p:graphicFrame>
 </p:spTree></p:cSld></p:sld>"#;

        let slide =
            super::parse_slide_for_markdown(xml, &Default::default()).expect("slide must parse");
        assert_eq!(
            slide.chart_rids,
            vec!["rId9".to_string()],
            "a start/end-form <c:chart> pointer was missed"
        );
        assert_eq!(
            slide.diagram_rids,
            vec!["rId12".to_string()],
            "a start/end-form <dgm:relIds> pointer was missed"
        );
    }

    /// The self-closing form, which is what real decks use, must be unchanged.
    #[test]
    fn the_self_closing_form_still_works() {
        let xml = br#"<?xml version="1.0"?>
<p:sld xmlns:p="p" xmlns:a="a" xmlns:c="c" xmlns:r="r">
 <p:cSld><p:spTree><p:graphicFrame><a:graphic><a:graphicData>
   <c:chart r:id="rId3"/>
 </a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#;
        let slide =
            super::parse_slide_for_markdown(xml, &Default::default()).expect("slide must parse");
        assert_eq!(slide.chart_rids, vec!["rId3".to_string()]);
    }
}
