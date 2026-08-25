//! The canonical streaming walker over `word/document.xml`.

use std::io::{BufReader, Read};

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::entities::read_event_folding_entities;

use super::block_model::{DocxBlock, DocxBlockKind};
use super::harvest::{harvest_blip_embed, harvest_image_alt, harvest_note_id};
use super::table_render::{render_table_inline, render_table_markdown, TableState};
use super::xml_text::{push_text, qname_eq};

/// Returns true when the paragraph style name indicates a list item without
/// requiring `<w:numPr>`. Matches Word's built-in list styles (ListNumber,
/// ListBullet, List, and their numbered variants like ListNumber2) as well as
/// the common single-word aliases used by older or third-party templates.
fn is_list_style(style: &str) -> bool {
    let lower = style.to_ascii_lowercase();
    // Exact or prefix matches: "listbullet", "listnumber", "list", "list2"…
    let prefixes = ["listbullet", "listnumber", "listparagraph", "list"];
    for prefix in &prefixes {
        if lower == *prefix || lower.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// A body-level `<w:altChunk>` placeholder, resolved later in
/// `parse_docx_blocks` (which, unlike this walker, holds the archive).
fn alt_chunk_placeholder(rid: String) -> DocxBlock {
    DocxBlock {
        kind: DocxBlockKind::Paragraph,
        text: String::new(),
        has_drawing: false,
        is_list: false,
        list_level: 0,
        heading_style: None,
        outline_level: None,
        page_break: false,
        section_break: false,
        rendered_page_break: false,
        image_alt: None,
        image_rid: None,
        images: Vec::new(),
        footnote_refs: Vec::new(),
        endnote_refs: Vec::new(),
        num_id: None,
        hyperlinks: Vec::new(),
        alt_chunk_rid: Some(rid),
    }
}

pub(super) fn parse_document_xml_blocks_streaming<R: Read>(
    reader_src: R,
) -> Result<Vec<DocxBlock>, String> {
    let mut reader = Reader::from_reader(BufReader::new(reader_src));

    let mut buf = Vec::new();
    let mut blocks: Vec<DocxBlock> = Vec::new();

    let mut in_text = false;
    // One <w:t> can arrive as several events, because quick-xml reports each
    // entity reference separately. Accumulate the element's text verbatim and
    // route it onward once, at </w:t> — joining per event would insert spaces
    // inside words ("AT&amp;T" -> "AT & T").
    let mut wt_buf = String::new();
    let mut in_paragraph = false;

    let mut para_text = String::new();
    let mut para_sub_texts: Vec<String> = Vec::new();
    let mut para_is_list = false;
    let mut para_list_level: u8 = 0;
    let mut para_has_drawing = false;
    let mut para_has_page_break = false;
    let mut para_has_section_break = false;
    let mut para_has_rendered_break = false;
    let mut para_style: Option<String> = None;
    let mut para_outline_lvl: Option<u32> = None;
    let mut para_image_alt: Option<String> = None;
    let mut para_image_rid: Option<String> = None;
    let mut para_images: Vec<(String, Option<String>)> = Vec::new();
    let mut pending_alt: Option<String> = None;
    let mut para_footnote_refs: Vec<String> = Vec::new();
    let mut para_endnote_refs: Vec<String> = Vec::new();
    let mut para_num_id: Option<u32> = None;
    let mut in_hyperlink = false;
    let mut hyperlink_rid = String::new();
    let mut hyperlink_text = String::new();
    let mut para_hyperlinks: Vec<(String, String)> = Vec::new();
    // Depth counter for `<w:drawing>` so we only harvest alt attributes
    // from `<wp:docPr>` / `<pic:cNvPr>` while we are actually inside a
    // drawing (those local names also appear in shape XML elsewhere).
    let mut drawing_depth: u32 = 0;
    let mut in_run = false;
    // Depth inside `<w:rt>`, the phonetic-reading half of a ruby annotation
    // (ECMA-376 §17.3.3.25). Its `<w:r><w:t>` looks exactly like ordinary run
    // content to this walker, so the reading was emitted as body text *ahead of*
    // the word it annotates: `<w:ruby>` over 漢字 yielded "ふりがな 漢字". That is
    // corrupted output, not missing output — the base text is what the document
    // says, and the reading is a gloss on it.
    let mut ruby_rt_depth: usize = 0;
    let mut in_rpr = false;
    let mut cur_bold = false;
    let mut cur_italic = false;
    let mut cur_run_text = String::new();

    // Stack of in-progress tables. Empty when we're at top-level body
    // content. Pushed on `<w:tbl>` Start, popped on `<w:tbl>` End. Nested
    // tables push additional states without disturbing parent state.
    let mut table_stack: Vec<TableState> = Vec::new();
    // Images inside table cells. Cell paragraphs deliberately do not set
    // `in_paragraph` (the table walker owns them), so the paragraph-level blip
    // harvest never sees them and every picture in a table was dropped. (#71)
    let mut table_images: Vec<(String, Option<String>)> = Vec::new();
    let mut table_drawing_depth: i32 = 0;
    let mut table_pending_alt: Option<String> = None;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                if qname_eq(name, b"tbl") {
                    table_stack.push(TableState::default());
                } else if qname_eq(name, b"tr") {
                    if let Some(top) = table_stack.last_mut() {
                        top.current_row.clear();
                        top.in_header_row = false;
                        top.in_tr_pr = false;
                    }
                } else if qname_eq(name, b"trPr") {
                    if let Some(top) = table_stack.last_mut() {
                        top.in_tr_pr = true;
                    }
                } else if qname_eq(name, b"tblHeader") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_tr_pr {
                            top.in_header_row = true;
                        }
                    }
                } else if qname_eq(name, b"tc") {
                    if let Some(top) = table_stack.last_mut() {
                        top.in_cell = true;
                        top.current_cell.clear();
                        top.cell_span = 1;
                        top.cur_cell_is_vmerge_continuation = false;
                    }
                } else if qname_eq(name, b"gridSpan") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let raw =
                                        String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                    if let Ok(v) = raw.trim().parse::<usize>() {
                                        if v > 1 {
                                            top.cell_span = v;
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                } else if qname_eq(name, b"vMerge") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            // Check for w:val="restart" — absent val or val≠"restart" means continuation
                            let mut is_restart = false;
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let v = String::from_utf8_lossy(attr.value.as_ref());
                                    is_restart = v.trim() == "restart";
                                    break;
                                }
                            }
                            top.cur_cell_is_vmerge_continuation = !is_restart;
                        }
                    }
                } else if qname_eq(name, b"p") {
                    if table_stack.is_empty() {
                        in_paragraph = true;
                        para_text.clear();
                        para_sub_texts.clear();
                        para_is_list = false;
                        para_list_level = 0;
                        para_has_drawing = false;
                        para_has_page_break = false;
                        para_has_section_break = false;
                        para_has_rendered_break = false;
                        para_style = None;
                        para_outline_lvl = None;
                        para_image_alt = None;
                        para_image_rid = None;
                        para_images.clear();
                        pending_alt = None;
                        para_footnote_refs.clear();
                        para_endnote_refs.clear();
                        para_num_id = None;
                        in_hyperlink = false;
                        hyperlink_rid.clear();
                        hyperlink_text.clear();
                        para_hyperlinks.clear();
                        drawing_depth = 0;
                        in_run = false;
                        in_rpr = false;
                        cur_bold = false;
                        cur_italic = false;
                        cur_run_text.clear();
                    }
                } else if qname_eq(name, b"numPr") && in_paragraph {
                    para_is_list = true;
                } else if qname_eq(name, b"br") && in_paragraph {
                    let mut is_page = false;
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"type") {
                            let value = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_ascii_lowercase();
                            if value == "page" {
                                para_has_page_break = true;
                                is_page = true;
                            }
                            break;
                        }
                    }
                    if !is_page {
                        // Soft line break — flush accumulated text as a sub-segment.
                        let flushed = std::mem::take(&mut para_text).trim().to_string();
                        if !flushed.is_empty() {
                            para_sub_texts.push(flushed);
                        }
                    }
                } else if qname_eq(name, b"sectPr") && in_paragraph {
                    para_has_section_break = true;
                } else if qname_eq(name, b"lastRenderedPageBreak") && in_paragraph {
                    para_has_rendered_break = true;
                } else if qname_eq(name, b"drawing") && !table_stack.is_empty() {
                    table_drawing_depth = table_drawing_depth.saturating_add(1);
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && table_drawing_depth > 0
                {
                    if let Some(alt) = harvest_image_alt(&e) {
                        table_pending_alt.get_or_insert(alt);
                    }
                } else if qname_eq(name, b"blip") && table_drawing_depth > 0 {
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if !table_images.iter().any(|(r, _)| *r == rid) {
                            table_images.push((rid, table_pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"drawing") && in_paragraph {
                    para_has_drawing = true;
                    drawing_depth = drawing_depth.saturating_add(1);
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && in_paragraph
                    && drawing_depth > 0
                {
                    // Alt text belongs to the drawing it sits in, so remember it
                    // for the next blip rather than only for the paragraph's
                    // first image.
                    if let Some(alt) = harvest_image_alt(&e) {
                        if para_image_alt.is_none() {
                            para_image_alt = Some(alt.clone());
                        }
                        pending_alt.get_or_insert(alt);
                    }
                } else if qname_eq(name, b"blip") && in_paragraph && drawing_depth > 0 {
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if para_image_rid.is_none() {
                            para_image_rid = Some(rid.clone());
                        }
                        // Keep EVERY blip: a paragraph can hold a gallery, and
                        // the first one may be a format we cannot decode. (#13)
                        if !para_images.iter().any(|(r, _)| *r == rid) {
                            para_images.push((rid, pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"footnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_footnote_refs.push(id);
                    }
                } else if qname_eq(name, b"endnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_endnote_refs.push(id);
                    }
                } else if qname_eq(name, b"pStyle") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let v = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            para_style = Some(v);
                            break;
                        }
                    }
                } else if qname_eq(name, b"ilvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u8>() {
                                para_list_level = v;
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"numId") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_num_id = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"hyperlink") && in_paragraph {
                    in_hyperlink = true;
                    hyperlink_text.clear();
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"id") {
                            hyperlink_rid = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            break;
                        }
                    }
                } else if qname_eq(name, b"r") && in_paragraph {
                    in_run = true;
                    cur_bold = false;
                    cur_italic = false;
                    cur_run_text.clear();
                } else if qname_eq(name, b"rPr") && in_run {
                    in_rpr = true;
                } else if qname_eq(name, b"outlineLvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_outline_lvl = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"rt") {
                    ruby_rt_depth += 1;
                } else if qname_eq(name, b"t") {
                    in_text = true;
                    wt_buf.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                if qname_eq(name, b"altChunk") {
                    // A body-level sibling of `<w:p>`; the walker only emits a
                    // block at `</w:p>` or `</w:tbl>`, so this produced nothing
                    // at all. Only at body level for now — an altChunk inside a
                    // table cell is legal but vanishingly rare, and splicing
                    // into a cell needs different handling.
                    if table_stack.is_empty() && !in_paragraph {
                        for attr in e.attributes().flatten() {
                            if qname_eq(attr.key, b"id") {
                                let rid = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                if !rid.is_empty() {
                                    blocks.push(alt_chunk_placeholder(rid));
                                }
                                break;
                            }
                        }
                    }
                } else if qname_eq(name, b"numPr") && in_paragraph {
                    para_is_list = true;
                } else if qname_eq(name, b"tblHeader") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_tr_pr {
                            top.in_header_row = true;
                        }
                    }
                } else if qname_eq(name, b"gridSpan") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let raw =
                                        String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                    if let Ok(v) = raw.trim().parse::<usize>() {
                                        if v > 1 {
                                            top.cell_span = v;
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                } else if qname_eq(name, b"vMerge") {
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            // Check for w:val="restart" — absent val or val≠"restart" means continuation
                            let mut is_restart = false;
                            for attr in e.attributes().flatten() {
                                if qname_eq(attr.key, b"val") {
                                    let v = String::from_utf8_lossy(attr.value.as_ref());
                                    is_restart = v.trim() == "restart";
                                    break;
                                }
                            }
                            top.cur_cell_is_vmerge_continuation = !is_restart;
                        }
                    }
                } else if qname_eq(name, b"br") && in_paragraph {
                    let mut is_page = false;
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"type") {
                            let value = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_ascii_lowercase();
                            if value == "page" {
                                para_has_page_break = true;
                                is_page = true;
                            }
                            break;
                        }
                    }
                    if !is_page {
                        let flushed = std::mem::take(&mut para_text).trim().to_string();
                        if !flushed.is_empty() {
                            para_sub_texts.push(flushed);
                        }
                    }
                } else if qname_eq(name, b"sectPr") && in_paragraph {
                    para_has_section_break = true;
                } else if qname_eq(name, b"lastRenderedPageBreak") && in_paragraph {
                    para_has_rendered_break = true;
                } else if qname_eq(name, b"drawing") && in_paragraph {
                    para_has_drawing = true;
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && in_paragraph
                    && drawing_depth > 0
                {
                    // Alt text belongs to the drawing it sits in, so remember it
                    // for the next blip rather than only for the paragraph's
                    // first image.
                    if let Some(alt) = harvest_image_alt(&e) {
                        if para_image_alt.is_none() {
                            para_image_alt = Some(alt.clone());
                        }
                        pending_alt.get_or_insert(alt);
                    }
                } else if (qname_eq(name, b"docPr") || qname_eq(name, b"cNvPr"))
                    && table_drawing_depth > 0
                {
                    if let Some(alt) = harvest_image_alt(&e) {
                        table_pending_alt.get_or_insert(alt);
                    }
                } else if qname_eq(name, b"blip") && table_drawing_depth > 0 {
                    // `<a:blip/>` is self-closing, so it only ever reaches the
                    // Empty arm — the Start-arm branch never sees it. (#71)
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if !table_images.iter().any(|(r, _)| *r == rid) {
                            table_images.push((rid, table_pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"blip") && in_paragraph && drawing_depth > 0 {
                    if let Some(rid) = harvest_blip_embed(&e) {
                        if para_image_rid.is_none() {
                            para_image_rid = Some(rid.clone());
                        }
                        // Keep EVERY blip: a paragraph can hold a gallery, and
                        // the first one may be a format we cannot decode. (#13)
                        if !para_images.iter().any(|(r, _)| *r == rid) {
                            para_images.push((rid, pending_alt.take()));
                        }
                    }
                } else if qname_eq(name, b"footnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_footnote_refs.push(id);
                    }
                } else if qname_eq(name, b"endnoteReference") && in_paragraph {
                    if let Some(id) = harvest_note_id(&e) {
                        para_endnote_refs.push(id);
                    }
                } else if qname_eq(name, b"pStyle") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let v = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            para_style = Some(v);
                            break;
                        }
                    }
                } else if qname_eq(name, b"ilvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u8>() {
                                para_list_level = v;
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"numId") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_num_id = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"outlineLvl") && in_paragraph {
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            let raw = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                para_outline_lvl = Some(v);
                            }
                            break;
                        }
                    }
                } else if qname_eq(name, b"b") && in_rpr {
                    let mut val = String::new();
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            val = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            break;
                        }
                    }
                    cur_bold = val != "false" && val != "0";
                } else if qname_eq(name, b"i") && in_rpr {
                    let mut val = String::new();
                    for attr in e.attributes().flatten() {
                        if qname_eq(attr.key, b"val") {
                            val = String::from_utf8_lossy(attr.value.as_ref())
                                .trim()
                                .to_string();
                            break;
                        }
                    }
                    cur_italic = val != "false" && val != "0";
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                if qname_eq(name, b"rt") {
                    ruby_rt_depth = ruby_rt_depth.saturating_sub(1);
                } else if qname_eq(name, b"t") {
                    in_text = false;
                    // Always taken, so `wt_buf` is cleared for the next run —
                    // then discarded inside `<w:rt>`, keeping `<w:rubyBase>`,
                    // which is the actual word. `push_text` ignores an empty
                    // piece, so every routing branch below is a no-op for it.
                    // (Not `continue`: that would skip this loop's `buf.clear()`
                    // and let quick-xml's buffer grow for the whole document.)
                    let txt = std::mem::take(&mut wt_buf);
                    let txt = if ruby_rt_depth > 0 {
                        String::new()
                    } else {
                        txt
                    };
                    if let Some(top) = table_stack.last_mut() {
                        if top.in_cell {
                            push_text(&mut top.current_cell, &txt);
                        }
                    } else if in_paragraph {
                        if in_run {
                            push_text(&mut cur_run_text, &txt);
                            if in_hyperlink {
                                push_text(&mut hyperlink_text, &txt);
                            }
                        } else {
                            push_text(&mut para_text, &txt);
                            if in_hyperlink {
                                push_text(&mut hyperlink_text, &txt);
                            }
                        }
                    }
                } else if qname_eq(name, b"drawing") {
                    drawing_depth = drawing_depth.saturating_sub(1);
                    // Only unwind the table counter for drawings that actually
                    // opened inside a table — decrementing on every </w:drawing>
                    // drove it negative, so the `> 0` guard below never passed
                    // and no table image was ever harvested.
                    if table_drawing_depth > 0 {
                        table_drawing_depth -= 1;
                        table_pending_alt = None;
                    }
                } else if qname_eq(name, b"hyperlink") && in_paragraph {
                    let anchor = hyperlink_text.trim().to_string();
                    if !anchor.is_empty() && !hyperlink_rid.is_empty() {
                        para_hyperlinks.push((anchor, hyperlink_rid.clone()));
                    }
                    in_hyperlink = false;
                    hyperlink_rid.clear();
                    hyperlink_text.clear();
                } else if qname_eq(name, b"rPr") {
                    in_rpr = false;
                } else if qname_eq(name, b"r") && in_paragraph {
                    if !cur_run_text.is_empty() {
                        let formatted = match (cur_bold, cur_italic) {
                            (true, true) => format!("***{}***", cur_run_text),
                            (true, false) => format!("**{}**", cur_run_text),
                            (false, true) => format!("*{}*", cur_run_text),
                            (false, false) => cur_run_text.clone(),
                        };
                        push_text(&mut para_text, &formatted);
                    }
                    in_run = false;
                    in_rpr = false;
                    cur_bold = false;
                    cur_italic = false;
                    cur_run_text.clear();
                } else if qname_eq(name, b"trPr") {
                    if let Some(top) = table_stack.last_mut() {
                        top.in_tr_pr = false;
                    }
                } else if qname_eq(name, b"tc") {
                    if let Some(top) = table_stack.last_mut() {
                        let col_index = top.current_row.len();
                        let raw_cell = std::mem::take(&mut top.current_cell).trim().to_string();

                        let cell = if top.cur_cell_is_vmerge_continuation {
                            // Repeat content from the cell above this column position.
                            top.vmerge_col_content
                                .get(col_index)
                                .cloned()
                                .unwrap_or_default()
                        } else {
                            raw_cell.clone()
                        };

                        let span = top.cell_span.max(1);
                        for i in 0..span {
                            // Update vmerge_col_content for each spanned column
                            let abs_col = col_index + i;
                            if !top.cur_cell_is_vmerge_continuation {
                                if top.vmerge_col_content.len() <= abs_col {
                                    top.vmerge_col_content.resize(abs_col + 1, String::new());
                                }
                                top.vmerge_col_content[abs_col] = raw_cell.clone();
                            }
                            top.current_row.push(cell.clone());
                        }

                        top.in_cell = false;
                        top.cell_span = 1;
                        top.cur_cell_is_vmerge_continuation = false;
                    }
                } else if qname_eq(name, b"tr") {
                    if let Some(top) = table_stack.last_mut() {
                        if !top.current_row.is_empty() {
                            let row = std::mem::take(&mut top.current_row);
                            top.header_row_flags.push(top.in_header_row);
                            top.rows.push(row);
                        }
                        top.in_header_row = false;
                        top.in_tr_pr = false;
                    }
                } else if qname_eq(name, b"tbl") {
                    if let Some(state) = table_stack.pop() {
                        if let Some(parent) = table_stack.last_mut() {
                            // Nested table: inline-flatten into the cell
                            // currently being built in the parent.
                            let inline = render_table_inline(&state);
                            if !inline.is_empty() {
                                if !parent.current_cell.is_empty()
                                    && !parent.current_cell.ends_with(' ')
                                {
                                    parent.current_cell.push(' ');
                                }
                                parent.current_cell.push_str(&inline);
                            }
                        } else {
                            let rendered = render_table_markdown(&state);
                            // A table whose cells hold only pictures renders as
                            // empty text — but it still has content. Emitting
                            // nothing here discarded the images with it. (#71)
                            if !rendered.is_empty() || !table_images.is_empty() {
                                blocks.push(DocxBlock {
                                    kind: DocxBlockKind::Table,
                                    text: rendered,
                                    has_drawing: !table_images.is_empty(),
                                    is_list: false,
                                    list_level: 0,
                                    heading_style: None,
                                    outline_level: None,
                                    page_break: false,
                                    section_break: false,
                                    rendered_page_break: false,
                                    image_alt: None,
                                    image_rid: None,
                                    images: std::mem::take(&mut table_images),
                                    footnote_refs: Vec::new(),
                                    endnote_refs: Vec::new(),
                                    num_id: None,
                                    hyperlinks: Vec::new(),
                                    alt_chunk_rid: None,
                                });
                            }
                        }
                    }
                } else if qname_eq(name, b"p") {
                    if in_paragraph {
                        // Also treat style-named list paragraphs (e.g. "ListNumber",
                        // "ListBullet") as list items even when <w:numPr> is absent.
                        let style_ref = para_style.as_deref().unwrap_or("");
                        let is_list = para_is_list || is_list_style(style_ref);

                        if para_sub_texts.is_empty() {
                            // Normal case — no soft breaks, emit one block.
                            blocks.push(DocxBlock {
                                kind: DocxBlockKind::Paragraph,
                                text: std::mem::take(&mut para_text),
                                has_drawing: para_has_drawing,
                                is_list,
                                list_level: para_list_level,
                                heading_style: para_style.take(),
                                outline_level: para_outline_lvl.take(),
                                page_break: para_has_page_break,
                                section_break: para_has_section_break,
                                rendered_page_break: para_has_rendered_break,
                                image_alt: para_image_alt.take(),
                                image_rid: para_image_rid.take(),
                                images: std::mem::take(&mut para_images),
                                footnote_refs: std::mem::take(&mut para_footnote_refs),
                                endnote_refs: std::mem::take(&mut para_endnote_refs),
                                num_id: para_num_id,
                                hyperlinks: std::mem::take(&mut para_hyperlinks),
                                alt_chunk_rid: None,
                            });
                        } else {
                            // Paragraph had soft line breaks — emit one block per segment.
                            // Flush any trailing text after the last <w:br/>.
                            let tail = std::mem::take(&mut para_text).trim().to_string();
                            if !tail.is_empty() {
                                para_sub_texts.push(tail);
                            }
                            for (i, sub_text) in para_sub_texts.drain(..).enumerate() {
                                blocks.push(DocxBlock {
                                    kind: DocxBlockKind::Paragraph,
                                    text: sub_text,
                                    // Drawings and break signals only belong to the first
                                    // segment; subsequent ones are plain text continuations.
                                    has_drawing: para_has_drawing && i == 0,
                                    is_list,
                                    list_level: para_list_level,
                                    heading_style: para_style.clone(),
                                    outline_level: para_outline_lvl,
                                    page_break: para_has_page_break && i == 0,
                                    section_break: para_has_section_break && i == 0,
                                    rendered_page_break: para_has_rendered_break && i == 0,
                                    image_alt: if i == 0 { para_image_alt.clone() } else { None },
                                    image_rid: if i == 0 { para_image_rid.clone() } else { None },
                                    images: if i == 0 {
                                        para_images.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    footnote_refs: if i == 0 {
                                        para_footnote_refs.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    endnote_refs: if i == 0 {
                                        para_endnote_refs.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    num_id: para_num_id,
                                    hyperlinks: if i == 0 {
                                        para_hyperlinks.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    alt_chunk_rid: None,
                                });
                            }
                            // Clear shared fields after all sub-blocks are emitted.
                            para_style = None;
                            para_outline_lvl = None;
                            para_image_alt = None;
                            para_footnote_refs.clear();
                            para_endnote_refs.clear();
                            para_hyperlinks.clear();
                        }

                        in_paragraph = false;
                        para_is_list = false;
                        para_list_level = 0;
                        para_num_id = None;
                        in_hyperlink = false;
                        hyperlink_rid.clear();
                        hyperlink_text.clear();
                        para_has_drawing = false;
                        para_has_page_break = false;
                        para_has_section_break = false;
                        para_has_rendered_break = false;
                        para_image_rid = None;
                        drawing_depth = 0;
                        in_run = false;
                        in_rpr = false;
                        cur_bold = false;
                        cur_italic = false;
                        cur_run_text.clear();
                    } else if let Some(top) = table_stack.last_mut() {
                        // Separate paragraphs within a table cell with a
                        // single space so multi-paragraph cells stay legible.
                        if top.in_cell
                            && !top.current_cell.is_empty()
                            && !top.current_cell.ends_with(' ')
                        {
                            top.current_cell.push(' ');
                        }
                    }
                }
            }
            Ok(Event::Text(t)) if in_text => {
                let txt = match t.decode() {
                    Ok(v) => v.into_owned(),
                    Err(_) => String::new(),
                };
                wt_buf.push_str(&txt);
            }
            Ok(Event::CData(t)) if in_text => {
                let txt = String::from_utf8_lossy(t.as_ref());
                wt_buf.push_str(&txt);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse word/document.xml stream: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(blocks)
}
