//! PPTX -> markdown entry points. The implementation lives in the
//! `md_blocks` / `md_slide_parse` / `md_rels` / `md_render` sibling modules.

use std::collections::HashMap;

use super::common::{parse_presentation_sections, read_zip_entry};
use super::md_blocks::{append_chart_blocks, append_diagram_blocks, SlideMarkdownContent};
use super::md_rels::{
    extract_notes_text, extract_presentation_title, parse_slide_rels, parse_slide_rels_with_images,
};
use super::md_render::{slide_to_markdown, slide_to_markdown_with_images};
use super::md_slide_parse::parse_slide_for_markdown;

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

pub(super) fn to_markdown_with_images(
    bytes: &[u8],
) -> Result<crate::chunk::MarkdownWithImages, String> {
    use super::common::{collect_slide_names, open_pptx};
    let mut archive = open_pptx(bytes).map_err(|e| format!("Not a valid PPTX zip: {e}"))?;
    let pres_title = extract_presentation_title(&mut archive);
    let sections = parse_presentation_sections(&mut archive).unwrap_or_default();
    let slide_names = collect_slide_names(&archive);

    let mut slides: Vec<(usize, SlideMarkdownContent)> = Vec::new();
    let mut slide_image_rids: std::collections::HashMap<
        usize,
        std::collections::HashMap<String, String>,
    > = std::collections::HashMap::new();
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
    let mut section_starts: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    for (section_name, slide_nums) in &sections {
        if let Some(&first) = slide_nums.first() {
            section_starts.insert(first, section_name.clone());
        }
    }

    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    for (slide_num, slide) in &slides {
        if let Some(section_name) = section_starts.get(slide_num) {
            parts.push(format!("# {}", section_name.trim()));
        }
        let image_rids = slide_image_rids.get(slide_num).cloned().unwrap_or_default();
        let slide_md = slide_to_markdown_with_images(
            *slide_num,
            slide,
            &image_rids,
            &mut archive,
            &mut image_out,
        );
        if !slide_md.trim().is_empty() {
            parts.push(slide_md);
        }
    }
    Ok((parts.join("\n\n---\n\n").trim().to_string(), image_out))
}
