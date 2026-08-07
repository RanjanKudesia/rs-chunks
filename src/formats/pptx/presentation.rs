//! `ppt/presentation.xml` parsing: slide order and named sections.

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::entities::read_event_folding_entities;

use super::archive::{read_zip_entry, PptxArchive};
use super::xml_util::{attr_value, local_name};

// ── Presentation section parser ───────────────────────────────────────────────

/// Reads `ppt/presentation.xml` and returns ordered slide IDs plus any named
/// sections.  Returns (slide_id_order, sections) where each section is
/// (section_name, Vec<slide_position_1based>).
pub fn parse_presentation_sections(
    archive: &mut PptxArchive,
) -> Result<Vec<(String, Vec<usize>)>, String> {
    let xml_bytes = read_zip_entry(archive, "ppt/presentation.xml")?;
    let mut reader = Reader::from_reader(xml_bytes.as_slice());
    let mut buf = Vec::new();

    // First pass: build ordered slide ID list from <p:sldIdLst>
    let mut slide_id_order: Vec<u32> = Vec::new();
    // Second pass: read section definitions
    let mut sections: Vec<(String, Vec<u32>)> = Vec::new();
    let mut in_sld_id_lst = false;
    let mut in_section_lst = false;
    let mut current_section_name: Option<String> = None;
    let mut current_section_ids: Vec<u32> = Vec::new();
    let mut in_section_sld_id_lst = false;

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        let mut is_entity = false;
        match read_event_folding_entities!(reader, &mut buf, &mut spill, &mut is_entity) {
            Ok(Event::Eof) => break,
            Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local = local_name(e.name());
                match local.as_slice() {
                    b"sldIdLst" => {
                        if in_section_lst {
                            in_section_sld_id_lst = true;
                        } else {
                            in_sld_id_lst = true;
                        }
                    }
                    b"sldId" if in_sld_id_lst && !in_section_lst => {
                        if let Some(id_str) = attr_value(e.attributes(), b"id") {
                            if let Ok(id) = id_str.parse::<u32>() {
                                slide_id_order.push(id);
                            }
                        }
                    }
                    b"sectionLst" => in_section_lst = true,
                    b"section" if in_section_lst => {
                        if let Some(prev_name) = current_section_name.take() {
                            if !current_section_ids.is_empty() {
                                sections.push((prev_name, current_section_ids.clone()));
                                current_section_ids.clear();
                            }
                        }
                        current_section_name = attr_value(e.attributes(), b"name");
                    }
                    b"sldId" if in_section_sld_id_lst => {
                        if let Some(id_str) = attr_value(e.attributes(), b"id") {
                            if let Ok(id) = id_str.parse::<u32>() {
                                current_section_ids.push(id);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name());
                match local.as_slice() {
                    b"sldIdLst" if !in_section_lst => in_sld_id_lst = false,
                    b"sldIdLst" if in_section_lst => in_section_sld_id_lst = false,
                    b"sectionLst" => {
                        if let Some(name) = current_section_name.take() {
                            if !current_section_ids.is_empty() {
                                sections.push((name, current_section_ids.clone()));
                                current_section_ids.clear();
                            }
                        }
                        in_section_lst = false;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    if sections.is_empty() || slide_id_order.is_empty() {
        return Ok(Vec::new());
    }

    // Convert slide IDs to 1-based slide positions
    let result = sections
        .into_iter()
        .map(|(name, ids)| {
            let positions: Vec<usize> = ids
                .iter()
                .filter_map(|id| {
                    slide_id_order
                        .iter()
                        .position(|&oid| oid == *id)
                        .map(|p| p + 1)
                })
                .collect();
            (name, positions)
        })
        .filter(|(_, positions)| !positions.is_empty())
        .collect();

    Ok(result)
}
