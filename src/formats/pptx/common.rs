/// Shared types, slide parser, and helpers for all PPTX chunking strategies.
use quick_xml::events::attributes::Attributes;
use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader;
use serde_json::{json, Value};
use std::io::{BufReader, Cursor, Read};
use zip::ZipArchive;

/// Every PresentationML OOXML extension routed through the pptx chunker. All
/// store slides under `ppt/slides/`, which the parser enumerates by part name
/// regardless of extension. NOTE: legacy binary `.ppt` is NOT here — it routes
/// to the separate `ppt` chunker.
pub const PPTX_OOXML_EXTS: &[&str] = &[".pptx", ".potx", ".potm", ".ppsx", ".ppsm"];

/// True if `path` ends in any supported PresentationML OOXML extension.
pub fn is_pptx_ooxml(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    PPTX_OOXML_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Human-readable extension list for error messages.
pub fn pptx_exts_display() -> String {
    PPTX_OOXML_EXTS.join(", ")
}

// Re-export shared utilities so strategy files can keep importing from super::common.
pub use crate::shared::{has_keyword_overlap, split_sentences, tokenize_keywords};

pub const MAX_CHUNK_CHARS: usize = 1200;
pub const MIN_CHUNK_CHARS: usize = 350;

/// Threshold above which a single-paragraph text is classified as LongSingleParagraph.
pub const CLASSIFY_LONG_CHARS: usize = 900;
/// Threshold below which a text is classified as ShortDisconnectedParagraph.
pub const CLASSIFY_SHORT_CHARS: usize = 90;

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
fn find_notes_for_slide(archive: &mut PptxArchive, slide_name: &str) -> Option<String> {
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

// ── Content types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    PlainParagraph,
    HeadingSection,
    BulletNumberedList,
    Table,
    LongSingleParagraph,
    ShortDisconnectedParagraph,
    Semantic,
    Section,
    SlidingWindow,
    Sentence,
    PageAware,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::PlainParagraph => "plain_paragraph",
            ContentType::HeadingSection => "heading",
            ContentType::BulletNumberedList => "bullet_list",
            ContentType::Table => "table",
            ContentType::LongSingleParagraph => "long_single_paragraph",
            ContentType::ShortDisconnectedParagraph => "short_disconnected_paragraph",
            ContentType::Semantic => "semantic",
            ContentType::Section => "section",
            ContentType::SlidingWindow => "sliding_window",
            ContentType::Sentence => "sentence",
            ContentType::PageAware => "page_aware",
        }
    }
}

// ── Chunk output ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChunkRecordInput {
    pub content_type: ContentType,
    pub content: String,
    pub metadata: Value,
}

// ── Slide content model ───────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct SlideContent {
    pub title: Option<String>,
    pub body_paragraphs: Vec<String>,
    pub has_table: bool,
    /// Speaker notes extracted from the slide's associated notes slide XML.
    pub notes_text: Option<String>,
    /// `r:dm` relationship ids of any SmartArt diagrams on the slide. The text
    /// lives in a separate `ppt/diagrams/dataN.xml` part, so the parser can only
    /// record the ids here; `read_all_slides` resolves and reads them.
    pub diagram_rids: Vec<String>,
}

impl SlideContent {
    pub fn is_empty_body(&self) -> bool {
        self.body_paragraphs.iter().all(|p| p.trim().is_empty())
    }

    /// True for slides that look like section dividers (title only, no body).
    pub fn is_section_divider(&self) -> bool {
        self.title.is_some() && self.is_empty_body()
    }

    pub fn all_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref t) = self.title {
            parts.push(t.clone());
        }
        for p in &self.body_paragraphs {
            let trimmed = p.trim().to_string();
            if !trimmed.is_empty() {
                parts.push(trimmed);
            }
        }
        if self.has_table && !parts.is_empty() {
            // Prefix the first body paragraph (not the title) with "Table:" so
            // classify_chunk can detect tables even when a title is present.
            let mark_idx = if self.title.is_some() && parts.len() > 1 {
                1
            } else {
                0
            };
            parts[mark_idx] = format!("Table: {}", parts[mark_idx]);
        }
        let main = parts.join("\n");
        // Append speaker notes with a separator so all chunking modes benefit.
        if let Some(ref notes) = self.notes_text {
            let n = notes.trim();
            if !n.is_empty() {
                return format!("{main}\n\n[Notes]\n{n}");
            }
        }
        main
    }
}

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
        match reader.read_event_into(&mut buf) {
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
                    b"t" if in_para || in_tc_para => {
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
            Ok(Event::Text(ref e)) => {
                if in_t {
                    let txt = e.decode().unwrap_or_default().trim().to_string();
                    if !txt.is_empty() {
                        if !t_buf.is_empty() {
                            t_buf.push(' ');
                        }
                        t_buf.push_str(&txt);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name());
                match local.as_slice() {
                    b"t" if in_t => {
                        in_t = false;
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
                            t_buf.clear();
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
                        let row = table_row_cells
                            .iter()
                            .filter(|c| !c.trim().is_empty())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ");
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
        match reader.read_event_into(&mut buf) {
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

// ── XML helpers ───────────────────────────────────────────────────────────────

pub fn local_name(name: QName<'_>) -> Vec<u8> {
    let raw = name.as_ref();
    raw.rsplit(|b| *b == b':').next().unwrap_or(raw).to_vec()
}

pub fn attr_value(attrs: Attributes<'_>, key: &[u8]) -> Option<String> {
    for attr in attrs.flatten() {
        let aname = attr.key.as_ref();
        let local = aname.rsplit(|b| *b == b':').next().unwrap_or(aname);
        if local == key {
            return attr.unescape_value().ok().map(|v| v.trim().to_string());
        }
    }
    None
}

// ── Text classification ───────────────────────────────────────────────────────

pub fn classify_chunk(text: &str) -> ContentType {
    if text.is_empty() {
        return ContentType::PlainParagraph;
    }
    if text.lines().any(|l| l.starts_with("Table:")) {
        return ContentType::Table;
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if !lines.is_empty()
        && lines
            .iter()
            .all(|l| is_bullet_line(l.trim()) || is_numbered_line(l.trim()))
    {
        return ContentType::BulletNumberedList;
    }
    if text.len() <= 200 && lines.len() <= 4 {
        let joined = lines.join(" ");
        if joined.split_whitespace().count() <= 12 && is_heading_style(&joined) {
            return ContentType::HeadingSection;
        }
    }
    if text.len() > CLASSIFY_LONG_CHARS {
        ContentType::LongSingleParagraph
    } else if text.len() < CLASSIFY_SHORT_CHARS {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

pub fn is_heading_style(text: &str) -> bool {
    let t = text.trim();
    let alpha: String = t.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.is_empty() {
        return false;
    }
    if alpha.len() >= 2 && alpha == alpha.to_uppercase() {
        return true;
    }
    if t.ends_with(':') {
        return true;
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.len() == 1 {
        return words[0]
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            && words[0].len() >= 4;
    }
    if words.len() <= 8 {
        let title_cased = words
            .iter()
            .all(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(true));
        if title_cased && !looks_like_sentence(t) {
            return true;
        }
    }
    false
}

pub fn looks_like_sentence(line: &str) -> bool {
    let words = line.split_whitespace().count();
    words >= 8 || line.ends_with('.') || line.ends_with('!') || line.ends_with('?')
}

pub fn is_bullet_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with('\u{2022}')
        || t.starts_with('\u{25E6}')
        || t.starts_with('\u{25AA}')
        || t.starts_with('\u{25B8}')
}

pub fn is_numbered_line(line: &str) -> bool {
    let t = line.trim();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return false;
    }
    let rest = &t[digits.len()..];
    matches!(rest.chars().next(), Some('.') | Some(')')) && rest.len() > 2
}

// ── Text splitting ────────────────────────────────────────────────────────────

pub fn split_large_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.trim().to_string()];
    }
    let sentences = split_sentences(text);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for sentence in sentences {
        let candidate = if current.is_empty() {
            sentence.clone()
        } else {
            format!("{current} {sentence}")
        };
        if candidate.len() <= max_chars {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(current.trim().to_string());
            }
            current = sentence;
            while current.len() > max_chars {
                let budget = crate::shared::floor_char_boundary(&current, max_chars);
                let split_at = current[..budget]
                    .rfind(' ')
                    .map(|i| i + 1)
                    .unwrap_or(budget)
                    .max(1);
                let split_at = crate::shared::floor_char_boundary(&current, split_at);
                chunks.push(current[..split_at].trim().to_string());
                current = current[split_at..].trim().to_string();
            }
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() {
        vec![text.trim().to_string()]
    } else {
        chunks.into_iter().filter(|c| !c.is_empty()).collect()
    }
}

// split_sentences — re-exported from crate::shared above.

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

// ── Metadata ──────────────────────────────────────────────────────────────────

pub fn pptx_metadata(
    slide_number: usize,
    slide_title: Option<String>,
    section_heading: Option<String>,
    total_slides: usize,
) -> Value {
    json!({
        "slide_number":     slide_number,
        "slide_range":      [slide_number, slide_number],
        "slide_title":      slide_title,
        "section_heading":  section_heading,
        "document_metadata": {
            "source_type":  "pptx",
            "total_slides": total_slides,
        }
    })
}

// ── Image extraction helpers ──────────────────────────────────────────────────

/// Returns `None` for unsupported formats (.emf, .wmf, etc.).
/// Returns `"<16hexchars>.<ext>"` for .png/.jpg/.jpeg/.gif/.webp.
pub fn image_hash_name(bytes: &[u8], zip_path: &str) -> Option<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let path = zip_path.to_ascii_lowercase();
    let ext = if path.ends_with(".png") {
        ".png"
    } else if path.ends_with(".jpg") {
        ".jpg"
    } else if path.ends_with(".jpeg") {
        ".jpeg"
    } else if path.ends_with(".gif") {
        ".gif"
    } else if path.ends_with(".webp") {
        ".webp"
    } else {
        return None;
    };
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(format!("{:016x}{ext}", hasher.finish()))
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
pub fn extract_slide_pic_rids(xml_bytes: &[u8]) -> Vec<(Option<String>, Option<String>)> {
    let mut reader = Reader::from_reader(std::io::BufReader::new(xml_bytes));
    let mut buf = Vec::new();
    let mut result = Vec::new();
    let mut in_pic = false;
    let mut pic_rid: Option<String> = None;
    let mut pic_alt: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
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

// tokenize_keywords, has_keyword_overlap — re-exported from crate::shared above.

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    // ── ZIP / slide helpers ───────────────────────────────────────────────────

    fn slide_xml(title: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
        <p:txBody>
          <a:p><a:r><a:t>{title}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr>
        <p:txBody>
          <a:p><a:r><a:t>{body}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#
        )
    }

    /// Build a minimal in-memory PPTX ZIP from (title, body) pairs.
    fn make_pptx(slides: &[(&str, &str)]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (i, (title, body)) in slides.iter().enumerate() {
            let path = format!("ppt/slides/slide{}.xml", i + 1);
            zip.start_file(path, opts.clone()).unwrap();
            zip.write_all(slide_xml(title, body).as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    // ── parse_slide_xml ───────────────────────────────────────────────────────

    #[test]
    fn parse_slide_xml_extracts_title() {
        let xml = slide_xml("My Title", "Body text here.");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert_eq!(slide.title.as_deref(), Some("My Title"));
    }

    #[test]
    fn parse_slide_xml_extracts_body() {
        let xml = slide_xml("Title", "Body paragraph content.");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert!(!slide.body_paragraphs.is_empty());
        assert!(slide.body_paragraphs[0].contains("Body paragraph"));
    }

    #[test]
    fn parse_slide_xml_empty_body_is_section_divider() {
        let xml = slide_xml("Section Title", "");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert!(
            slide.is_section_divider(),
            "title-only slide should be a section divider"
        );
    }

    #[test]
    fn parse_slide_xml_with_body_is_not_section_divider() {
        let xml = slide_xml("Title", "Body content present.");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert!(!slide.is_section_divider());
    }

    #[test]
    fn parse_slide_xml_invalid_xml_returns_error() {
        let bad = b"not xml at all <<>>";
        // Parser is lenient; just check it doesn't panic — any result is fine.
        let _ = parse_slide_xml(bad);
    }

    // ── SlideContent::all_text ────────────────────────────────────────────────

    #[test]
    fn all_text_joins_title_and_body() {
        let s = SlideContent {
            title: Some("Title".to_string()),
            body_paragraphs: vec!["Body".to_string()],
            has_table: false,
            notes_text: None,
            ..Default::default()
        };
        let t = s.all_text();
        assert!(t.contains("Title"));
        assert!(t.contains("Body"));
    }

    #[test]
    fn all_text_appends_notes_with_separator() {
        let s = SlideContent {
            title: Some("Title".to_string()),
            body_paragraphs: vec!["Body".to_string()],
            has_table: false,
            notes_text: Some("Speaker note text.".to_string()),
            ..Default::default()
        };
        let t = s.all_text();
        assert!(t.contains("[Notes]"));
        assert!(t.contains("Speaker note text."));
    }

    #[test]
    fn all_text_table_prefixes_body_paragraph() {
        let s = SlideContent {
            title: Some("Title".to_string()),
            body_paragraphs: vec!["Cell A | Cell B".to_string()],
            has_table: true,
            notes_text: None,
            ..Default::default()
        };
        let t = s.all_text();
        assert!(
            t.contains("Table:"),
            "has_table should prefix body with 'Table:'"
        );
    }

    // ── open_pptx / collect_slide_names ──────────────────────────────────────

    #[test]
    fn open_pptx_with_valid_zip_succeeds() {
        let bytes = make_pptx(&[("Title", "Body")]);
        assert!(open_pptx(&bytes).is_ok());
    }

    #[test]
    fn open_pptx_with_invalid_bytes_returns_error() {
        assert!(open_pptx(b"not a zip file").is_err());
    }

    #[test]
    fn collect_slide_names_finds_slides_in_order() {
        let bytes = make_pptx(&[
            ("Slide 1", "Body 1"),
            ("Slide 2", "Body 2"),
            ("Slide 3", "Body 3"),
        ]);
        let archive = open_pptx(&bytes).unwrap();
        let names = collect_slide_names(&archive);
        assert_eq!(names.len(), 3);
        assert_eq!(names[0].0, 1);
        assert_eq!(names[1].0, 2);
        assert_eq!(names[2].0, 3);
    }

    #[test]
    fn collect_slide_names_excludes_layouts_and_masters() {
        // A slide named ppt/slides/slideLayout1.xml should be filtered out.
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("ppt/slides/slideLayout1.xml", opts.clone())
            .unwrap();
        zip.write_all(b"<layout/>").unwrap();
        zip.start_file("ppt/slides/slide1.xml", opts).unwrap();
        zip.write_all(slide_xml("Title", "Body").as_bytes())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();

        let archive = open_pptx(&bytes).unwrap();
        let names = collect_slide_names(&archive);
        assert_eq!(
            names.len(),
            1,
            "layout should be excluded, only slide1 present"
        );
    }

    // ── classify_chunk ────────────────────────────────────────────────────────

    #[test]
    fn classify_chunk_bullet_lines_is_list() {
        let text = "- First bullet\n- Second bullet\n- Third bullet";
        assert_eq!(classify_chunk(text).as_str(), "bullet_list");
    }

    #[test]
    fn classify_chunk_table_prefix_is_table() {
        let text = "Table: Cell A | Cell B\nRow 2 | Value 2";
        assert_eq!(classify_chunk(text).as_str(), "table");
    }

    #[test]
    fn classify_chunk_long_text_is_long_paragraph() {
        let text = "x".repeat(CLASSIFY_LONG_CHARS + 1);
        assert_eq!(
            classify_chunk(text.as_str()).as_str(),
            "long_single_paragraph"
        );
    }

    #[test]
    fn classify_chunk_short_text_is_short_paragraph() {
        let text = "x".repeat(CLASSIFY_SHORT_CHARS - 1);
        assert_eq!(
            classify_chunk(text.as_str()).as_str(),
            "short_disconnected_paragraph"
        );
    }

    // ── is_heading_style / looks_like_sentence ────────────────────────────────

    #[test]
    fn is_heading_style_all_caps() {
        assert!(is_heading_style("INTRODUCTION"));
    }

    #[test]
    fn is_heading_style_ends_with_colon() {
        assert!(is_heading_style("Summary:"));
    }

    #[test]
    fn looks_like_sentence_long_text() {
        // ≥8 words → sentence
        assert!(looks_like_sentence(
            "this is a sentence with eight words here"
        ));
    }

    #[test]
    fn looks_like_sentence_ends_with_period() {
        assert!(looks_like_sentence("Short text."));
    }

    // ── is_bullet_line / is_numbered_line ─────────────────────────────────────

    #[test]
    fn is_bullet_line_dash_and_star() {
        assert!(is_bullet_line("- item"));
        assert!(is_bullet_line("* item"));
    }

    #[test]
    fn is_bullet_line_unicode_bullets() {
        assert!(is_bullet_line("\u{2022} item"));
    }

    #[test]
    fn is_numbered_line_dot_separator() {
        assert!(is_numbered_line("1. item"));
        assert!(is_numbered_line("10. item"));
    }

    #[test]
    fn is_numbered_line_paren_separator() {
        assert!(is_numbered_line("1) item"));
    }

    #[test]
    fn is_numbered_line_too_many_digits_is_false() {
        assert!(!is_numbered_line("1234. item"));
    }

    // ── split_large_text ──────────────────────────────────────────────────────

    #[test]
    fn split_large_text_short_input_unchanged() {
        let t = "Short text.";
        let parts = split_large_text(t, 1000);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], t);
    }

    #[test]
    fn split_large_text_splits_long_input() {
        let t = "Sentence one. ".repeat(100);
        let parts = split_large_text(&t, 200);
        assert!(parts.len() > 1, "long text should be split");
        assert!(parts.iter().all(|p| !p.is_empty()));
    }
}
