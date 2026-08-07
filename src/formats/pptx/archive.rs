//! PPTX zip-archive access and speaker-notes lookup.

use std::io::{Cursor, Read};
use zip::ZipArchive;

use super::slide_xml::parse_slide_xml;

// ── Archive helpers ───────────────────────────────────────────────────────────

pub type PptxArchive = ZipArchive<Cursor<Vec<u8>>>;

pub fn open_pptx(bytes: &[u8]) -> Result<PptxArchive, String> {
    let cursor = Cursor::new(bytes.to_vec());
    ZipArchive::new(cursor).map_err(|e| format!("PPTX is not a valid zip: {e}"))
}

pub fn read_zip_entry(archive: &mut PptxArchive, name: &str) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| format!("Entry '{name}' not found in PPTX archive"))?;
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read '{name}': {e}"))?;
    Ok(buf)
}

/// Returns sorted (slide_number, zip_entry_name) pairs — layouts and masters excluded.
pub fn collect_slide_names(archive: &PptxArchive) -> Vec<(usize, String)> {
    let mut slides: Vec<(usize, String)> = (0..archive.len())
        .filter_map(|i| {
            let name = archive.name_for_index(i)?.to_string();
            parse_slide_number(&name).map(|n| (n, name))
        })
        .collect();
    slides.sort_by_key(|(n, _)| *n);
    slides
}

fn parse_slide_number(name: &str) -> Option<usize> {
    let stem = name
        .strip_prefix("ppt/slides/slide")?
        .strip_suffix(".xml")?;
    if stem.contains("Layout")
        || stem.contains("Master")
        || stem.contains("layout")
        || stem.contains("master")
    {
        return None;
    }
    stem.parse::<usize>().ok()
}

// ── Speaker-notes helpers ─────────────────────────────────────────────────────

/// Resolves a relative path (e.g. `../notesSlides/notesSlide1.xml`) against a
/// base directory (e.g. `ppt/slides`), returning the canonical archive path.
pub fn resolve_relative_path(base_dir: &str, relative: &str) -> String {
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

/// Locates the notes-slide archive path for `slide_name` by reading its .rels
/// file.  Returns `None` if the slide has no associated notes slide.
pub(super) fn find_notes_for_slide(archive: &mut PptxArchive, slide_name: &str) -> Option<String> {
    let last_slash = slide_name.rfind('/')?;
    let dir = &slide_name[..last_slash];
    let file = &slide_name[last_slash + 1..];
    let rels_path = format!("{}/_rels/{}.rels", dir, file);
    let xml_bytes = read_zip_entry(archive, &rels_path).ok()?;
    let content = std::str::from_utf8(&xml_bytes).ok()?;
    for chunk in content.split("<Relationship ") {
        // Must reference notesSlide but NOT notesSlideLayout.
        if chunk.contains("notesSlide") && !chunk.contains("notesSlideLayout") {
            if let Some(target_start) = chunk.find("Target=\"") {
                let rest = &chunk[target_start + 8..];
                if let Some(target_end) = rest.find('"') {
                    return Some(resolve_relative_path(dir, &rest[..target_end]));
                }
            }
        }
    }
    None
}

/// Extracts text from a notes-slide XML.  Reuses the slide parser; body
/// paragraphs contain the speaker notes (the title slot holds the image).
pub(super) fn parse_notes_xml(xml_bytes: &[u8]) -> Option<String> {
    let slide = parse_slide_xml(xml_bytes).ok()?;
    let text = slide
        .body_paragraphs
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}
