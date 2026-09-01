//! The slide XML parser and whole-deck slide reading.

use quick_xml::events::attributes::Attributes;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufReader;

use crate::entities::read_event_folding_entities;

use super::archive::{find_notes_for_slide, parse_notes_xml, read_zip_entry, PptxArchive};
use super::slide_model::SlideContent;
use super::xml_util::local_name;

// ── Slide XML parser ──────────────────────────────────────────────────────────

/// `<dgm:relIds r:dm="rId2" r:lo=… r:qs=… r:cs=…/>` — only `dm` (the *data
/// model*) carries text; the others are layout, quick-style and colours.
fn diagram_data_rid(attrs: Attributes<'_>) -> Option<String> {
    for attr in attrs.flatten() {
        let key = attr.key.as_ref();
        let local = key.rsplit(|b| *b == b':').next().unwrap_or(key);
        if local == b"dm" {
            let v = attr.unescape_value().ok()?.trim().to_string();
            return (!v.is_empty()).then_some(v);
        }
    }
    None
}

/// `<c:chart r:id="rId2"/>` inside a `<a:graphicData>` — the pointer to the
/// part holding the plotted data.
fn chart_rel_id(attrs: Attributes<'_>) -> Option<String> {
    for attr in attrs.flatten() {
        let key = attr.key.as_ref();
        let local = key.rsplit(|b| *b == b':').next().unwrap_or(key);
        if local == b"id" {
            let v = attr.unescape_value().ok()?.trim().to_string();
            return (!v.is_empty()).then_some(v);
        }
    }
    None
}

pub fn parse_slide_xml(xml_bytes: &[u8]) -> Result<SlideContent, String> {
    let mut reader = Reader::from_reader(BufReader::new(xml_bytes));
    let mut buf = Vec::new();
    let mut slide = SlideContent::default();
    let mut sp_depth: i32 = 0;
    let mut sp_is_title = false;
    let mut sp_ph_checked = false;
    let mut in_txbody = false;
    let mut in_para = false;
    let mut para_text = String::new();
    let mut shape_paragraphs: Vec<String> = Vec::new();
    let mut t_buf = String::new();
    let mut in_t = false;
    // Inside an <a:fld> whose cached value is slide chrome (slide number,
    // date/time, footer). The cache is whatever the value was at save time;
    // emitting it made a slide whose only content is its number render as
    // body text `- 10` (poi_2411 slide 10) and put stale numbers into notes.
    let mut in_chrome_fld = false;
    // ── Table-cell extraction state ──────────────────────────────────────────
    let mut in_tbl = false;
    let mut in_tc = false; // inside <a:tc>
    let mut in_tc_body = false; // inside table cell's <a:txBody>
    let mut in_tc_para = false; // inside table cell's <a:p>
    let mut tc_para_text = String::new();
    let mut tc_cell_paras: Vec<String> = Vec::new();
    let mut table_row_cells: Vec<String> = Vec::new();
    let mut table_all_rows: Vec<String> = Vec::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error in slide: {e}")),
            Ok(Event::Start(ref e)) => {
                let local = local_name(e.name());
                match local.as_slice() {
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
                        let (mut ph_type, mut ph_idx) = (None::<String>, None::<String>);
                        for attr in e.attributes().flatten() {
                            let aname = attr.key.as_ref();
                            let local = aname.rsplit(|b| *b == b':').next().unwrap_or(aname);
                            match local {
                                b"type" => {
                                    ph_type =
                                        attr.unescape_value().ok().map(|v| v.trim().to_string())
                                }
                                b"idx" => {
                                    ph_idx =
                                        attr.unescape_value().ok().map(|v| v.trim().to_string())
                                }
                                _ => {}
                            }
                        }
                        if let Some(t) = ph_type {
                            let t_lower = t.to_ascii_lowercase();
                            sp_is_title =
                                matches!(t_lower.as_str(), "title" | "ctrtitle" | "subtitle");
                        } else if ph_idx.as_deref() == Some("0") {
                            // No type attribute but idx=0 is the title placeholder by convention.
                            sp_is_title = true;
                        }
                    }
                    b"txBody" if sp_depth > 0 => in_txbody = true,
                    b"p" if in_txbody => {
                        in_para = true;
                        para_text.clear();
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
                    b"t" if (in_para || in_tc_para) && !in_chrome_fld => {
                        in_t = true;
                        t_buf.clear();
                    }
                    b"tbl" => {
                        slide.has_table = true;
                        in_tbl = true;
                        table_all_rows.clear();
                    }
                    b"tr" if in_tbl => table_row_cells.clear(),
                    b"tc" if in_tbl => {
                        in_tc = true;
                        tc_cell_paras.clear();
                    }
                    b"txBody" if in_tc => in_tc_body = true,
                    b"p" if in_tc_body => {
                        in_tc_para = true;
                        tc_para_text.clear();
                    }
                    // A SmartArt graphic's text is not in this file at all —
                    // only the pointer to the part that holds it.
                    b"relIds" => {
                        if let Some(rid) = diagram_data_rid(e.attributes()) {
                            slide.diagram_rids.push(rid);
                        }
                    }
                    // A chart contributes no text to the slide either — only
                    // the pointer to the part holding its numbers.
                    b"chart" => {
                        if let Some(rid) = chart_rel_id(e.attributes()) {
                            slide.chart_rids.push(rid);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = local_name(e.name());
                if local.as_slice() == b"relIds" {
                    if let Some(rid) = diagram_data_rid(e.attributes()) {
                        slide.diagram_rids.push(rid);
                    }
                }
                // `<c:chart r:id=…/>` is self-closing, so this is the arm that
                // actually fires for every chart in our fixtures.
                if local.as_slice() == b"chart" {
                    if let Some(rid) = chart_rel_id(e.attributes()) {
                        slide.chart_rids.push(rid);
                    }
                }
                if local.as_slice() == b"ph" && sp_depth > 0 && !sp_ph_checked {
                    sp_ph_checked = true;
                    let (mut ph_type, mut ph_idx) = (None::<String>, None::<String>);
                    for attr in e.attributes().flatten() {
                        let aname = attr.key.as_ref();
                        let local_attr = aname.rsplit(|b| *b == b':').next().unwrap_or(aname);
                        match local_attr {
                            b"type" => {
                                ph_type = attr.unescape_value().ok().map(|v| v.trim().to_string())
                            }
                            b"idx" => {
                                ph_idx = attr.unescape_value().ok().map(|v| v.trim().to_string())
                            }
                            _ => {}
                        }
                    }
                    if let Some(t) = ph_type {
                        let t_lower = t.to_ascii_lowercase();
                        sp_is_title = matches!(t_lower.as_str(), "title" | "ctrtitle" | "subtitle");
                    } else if ph_idx.as_deref() == Some("0") {
                        sp_is_title = true;
                    }
                }
            }
            // CDATA inside `<a:t>` is text. Without this arm it fell through
            // `_ => {}` and the whole paragraph vanished from `get_chunks`,
            // while `get_markdown` kept it — the third way these two parsers
            // have disagreed.
            Ok(Event::CData(ref e)) if in_t => {
                t_buf.push_str(&String::from_utf8_lossy(e.as_ref()));
            }
            Ok(Event::Text(ref e)) if in_t => {
                // One <a:t> arrives as several events when it contains
                // entity references, so concatenate verbatim here and let
                // the flush at </a:t> do the trimming and joining. Trimming
                // per event would put a space inside a word.
                t_buf.push_str(e.decode().unwrap_or_default().as_ref());
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name());
                match local.as_slice() {
                    b"fld" => in_chrome_fld = false,
                    b"t" if in_t => {
                        in_t = false;
                        let t_buf = std::mem::take(&mut t_buf).trim().to_string();
                        if !t_buf.is_empty() {
                            // Route accumulated text to the correct buffer.
                            if in_tc_para {
                                if !tc_para_text.is_empty() {
                                    tc_para_text.push(' ');
                                }
                                tc_para_text.push_str(&t_buf);
                            } else {
                                if !para_text.is_empty() {
                                    para_text.push(' ');
                                }
                                para_text.push_str(&t_buf);
                            }
                        }
                    }
                    b"p" if in_para => {
                        in_para = false;
                        let trimmed = para_text.trim().to_string();
                        if !trimmed.is_empty() {
                            shape_paragraphs.push(trimmed);
                        }
                        para_text.clear();
                    }
                    b"txBody" if in_txbody => in_txbody = false,
                    // ── Table-cell end events ──────────────────────────────────────────
                    b"p" if in_tc_para => {
                        in_tc_para = false;
                        let trimmed = tc_para_text.trim().to_string();
                        if !trimmed.is_empty() {
                            tc_cell_paras.push(trimmed);
                        }
                        tc_para_text.clear();
                    }
                    b"txBody" if in_tc_body => in_tc_body = false,
                    b"tc" if in_tc => {
                        in_tc = false;
                        let cell_text = tc_cell_paras.join(" ").trim().to_string();
                        table_row_cells.push(cell_text);
                        tc_cell_paras.clear();
                    }
                    b"tr" if in_tbl => {
                        // A blank cell is a POSITION, not noise. Filtering them
                        // shifted every column right of the gap one place left,
                        // so the value ended up under the wrong header.
                        // Measured on oxml_03_2006Calendar_TP10081921.potx:
                        // February 2006 starts on a Wednesday, and `get_chunks`
                        // filed the 1st under SUNDAY while `get_markdown` — which
                        // keeps blanks — had it right. Plausible, wrong, and
                        // undetectable downstream.
                        //
                        // An all-blank row is still dropped: that is a spacer,
                        // and emitting `|  |  |  |` would put noise in every
                        // retrieval chunk. One deliberate divergence from the
                        // markdown surface, which renders it.
                        let row = table_row_cells.join(" | ");
                        if !row.trim().is_empty() {
                            table_all_rows.push(row);
                        }
                        table_row_cells.clear();
                    }
                    b"tbl" if in_tbl => {
                        in_tbl = false;
                        let table_text = table_all_rows
                            .iter()
                            .filter(|r| !r.trim().is_empty())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !table_text.is_empty() {
                            slide.body_paragraphs.push(table_text);
                        }
                        table_all_rows.clear();
                    }
                    b"sp" if sp_depth > 0 => {
                        sp_depth -= 1;
                        if sp_depth == 0 {
                            let combined = shape_paragraphs.join("\n").trim().to_string();
                            if !combined.is_empty() {
                                if sp_is_title && slide.title.is_none() {
                                    slide.title = Some(combined);
                                } else {
                                    slide.body_paragraphs.push(combined);
                                }
                            }
                            shape_paragraphs.clear();
                            sp_is_title = false;
                            sp_ph_checked = false;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(slide)
}

/// Read and parse all slides from the archive. Returns (slide_number, SlideContent) pairs
/// in sorted order; does not filter out empty slides — callers decide what to skip.
pub fn read_all_slides(
    archive: &mut PptxArchive,
    slide_names: &[(usize, String)],
) -> Result<Vec<(usize, SlideContent)>, String> {
    let mut slides = Vec::with_capacity(slide_names.len());
    for (slide_num, name) in slide_names {
        let xml_bytes = read_zip_entry(archive, name)?;
        let mut slide = parse_slide_xml(&xml_bytes)?;
        // SmartArt keeps its text in a sibling part, so pull it in and append it
        // to the body — otherwise every diagram label is silently dropped.
        // Best-effort: a broken diagram must not fail the slide.
        for part in super::diagram::resolve_diagram_parts(archive, name, &slide.diagram_rids) {
            if let Ok(bytes) = read_zip_entry(archive, &part) {
                slide
                    .body_paragraphs
                    .extend(super::diagram::parse_diagram_xml(&bytes));
            }
        }
        // Charts, same shape. The rows are pushed as body paragraphs so every
        // chunking mode picks them up through `all_text()`.
        for part in super::chart::resolve_chart_parts(archive, name, &slide.chart_rids) {
            if let Ok(bytes) = read_zip_entry(archive, &part) {
                let rows = super::chart::parse_chart_xml(&bytes);
                if !rows.is_empty() {
                    slide.body_paragraphs.push("Chart".to_string());
                    for row in rows {
                        slide.body_paragraphs.push(row.join(" | "));
                    }
                }
            }
        }
        // Load speaker notes via the slide's .rels file (best-effort; ignore failures).
        if let Some(notes_path) = find_notes_for_slide(archive, name) {
            if let Ok(notes_bytes) = read_zip_entry(archive, &notes_path) {
                slide.notes_text = parse_notes_xml(&notes_bytes);
            }
        }
        slides.push((*slide_num, slide));
    }
    Ok(slides)
}

#[cfg(test)]
mod cdata_tests {
    /// CDATA inside `<a:t>` is text, and dropping it lost the whole paragraph.
    ///
    /// There was no `CData` arm, so the event fell through `_ => {}` and the
    /// paragraph vanished from `get_chunks` while `get_markdown` kept it — the
    /// third way these two parsers disagreed. No fixture contains CDATA (Power-
    /// Point never writes it), so only a synthetic input can pin this.
    #[test]
    fn cdata_in_a_text_run_is_not_dropped() {
        let xml = br#"<?xml version="1.0"?>
<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
 <p:sp><p:txBody>
  <a:p><a:r><a:t><![CDATA[Quarterly review notes]]></a:t></a:r></a:p>
  <a:p><a:r><a:t>before <![CDATA[middle]]> after</a:t></a:r></a:p>
 </p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#;

        let slide = super::parse_slide_xml(xml).expect("slide must parse");
        let text = slide.all_text();
        assert!(
            text.contains("Quarterly review notes"),
            "a CDATA-only paragraph was dropped: {text:?}"
        );
        assert!(
            text.contains("before middle after"),
            "CDATA mixed with text was dropped: {text:?}"
        );
    }
}
