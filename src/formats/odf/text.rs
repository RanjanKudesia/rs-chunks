//! Shared ODF `content.xml` walker → markdown. Handles both the text-document
//! flow (`.odt`) and the slide loop (`.odp`) with one stateful pass. ODF stores
//! text in `text:p`/`text:h`, lists in `text:list`, tables in `table:table`,
//! links in `text:a`, footnotes in `text:note`, and (odp) slides in `draw:page`.

use crate::entities::read_event_folding_entities;
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;

use super::container::OdfKind;

/// Collapse runs of whitespace to a single space and trim. ODF flow text treats
/// inter-element indentation whitespace (from pretty-printed content.xml) as
/// insignificant, exactly like HTML — so we normalise it the same way. Explicit
/// spacing comes from `text:s`/`text:tab` (→ space) and `text:line-break`.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out.trim().to_string()
}

/// Minimal XML entity decoder for the few places we scan raw XML (meta.xml).
pub fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// ODF's grid ceiling for a repeated cell, borrowed from the spreadsheet side
/// so one attacker-controlled count cannot allocate without bound.
const MAX_TABLE_COLS: usize = crate::formats::xlsx::common::MAX_SHEET_COLS;
/// Trailing empty cells past this are grid padding, not authored columns.
/// A row may legitimately declare `number-columns-repeated="16384"` on its last
/// empty cell purely to fill the sheet; materialising that is pointless.
const MAX_TRAILING_EMPTY_COLS: usize = 64;

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    in_cell: bool,
    cell_text: String,
    /// `table:number-columns-repeated` on the cell being read.
    cell_repeat: usize,
    /// Empty cells seen but not yet materialised — see `push_cell`.
    pending_empty: usize,
}

impl TableState {
    /// Add a cell, honouring `table:number-columns-repeated`.
    ///
    /// Empty cells are deferred rather than pushed: a run of them at the end of
    /// a row is grid padding, and a declared repeat of 16,384 would otherwise
    /// build a row no document actually has.
    fn push_cell(&mut self, cell: String, repeat: usize) {
        let repeat = repeat.max(1);
        if cell.is_empty() {
            self.pending_empty = self.pending_empty.saturating_add(repeat);
            return;
        }
        let pending = std::mem::take(&mut self.pending_empty);
        let room = MAX_TABLE_COLS.saturating_sub(self.current_row.len());
        for _ in 0..pending.min(room) {
            self.current_row.push(String::new());
        }
        let room = MAX_TABLE_COLS.saturating_sub(self.current_row.len());
        for _ in 0..repeat.min(room) {
            self.current_row.push(cell.clone());
        }
    }

    /// Close a row, keeping authored trailing columns and dropping padding.
    ///
    /// `pending_empty > 0` means the row HAD cells, they were merely empty — so
    /// they are materialised even when nothing non-empty followed. Dropping the
    /// row in that case made a table of entirely empty cells disappear:
    /// caught on `odftoolkit_simple-table.odt`, one row, one empty cell,
    /// n 1 -> 0.
    fn end_row(&mut self) {
        let pad = std::mem::take(&mut self.pending_empty).min(MAX_TRAILING_EMPTY_COLS);
        let room = MAX_TABLE_COLS.saturating_sub(self.current_row.len());
        for _ in 0..pad.min(room) {
            self.current_row.push(String::new());
        }
        if !self.current_row.is_empty() {
            self.rows.push(std::mem::take(&mut self.current_row));
        }
    }
}

/// `table:number-columns-repeated` on a cell element, clamped.
fn cell_repeat(e: &quick_xml::events::BytesStart) -> usize {
    attr(e, b"table:number-columns-repeated")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_TABLE_COLS)
}

struct Walker {
    blocks: Vec<String>,
    /// Inline text of the current paragraph/heading/cell.
    text: String,
    heading_level: Option<u8>,
    list_depth: usize,
    in_list_item: bool,
    ordered_stack: Vec<bool>,
    /// `draw:name` of the frame currently being walked, used as image alt text.
    pending_frame_name: Option<String>,
    /// Original basename → hashed image key, from the container.
    image_names: std::collections::HashMap<String, String>,
    /// `<text:list-style>` name -> numbers its items.
    list_styles: std::collections::HashMap<String, bool>,
    /// Open tables, innermost last.
    ///
    /// Was `Option<TableState>`: a nested `table:table` OVERWROTE the outer
    /// one, so the outer table's completed rows were dropped, the inner table
    /// was emitted as a top-level block outside its parent cell, and every
    /// remaining outer cell leaked into body text because `table` was then
    /// `None`. A stack flattens the inner table into its parent cell instead,
    /// which is what docx already does (`docx/table_render.rs`).
    tables: Vec<TableState>,
    // Footnotes collected for a trailing "## Notes" section.
    notes: Vec<String>,
    in_note_body: bool,
    note_buf: String,
    // ODP speaker notes for the current slide.
    in_pres_notes: bool,
    slide_notes: String,
    // Pending hyperlink: (href, index into `text` where the link text began).
    link: Option<(String, usize)>,
    slide_count: usize,
}

fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key || local(a.key.as_ref()) == local(key) {
            // Escaped in the file like every attribute value; `xlink:href` is
            // rendered straight into `[label](url)`, so it must be decoded.
            return Some(crate::entities::decode_attr(&a));
        }
    }
    None
}

impl Walker {
    fn new() -> Self {
        Walker {
            blocks: Vec::new(),
            text: String::new(),
            heading_level: None,
            list_depth: 0,
            in_list_item: false,
            ordered_stack: Vec::new(),
            pending_frame_name: None,
            image_names: std::collections::HashMap::new(),
            list_styles: std::collections::HashMap::new(),
            tables: Vec::new(),
            notes: Vec::new(),
            in_note_body: false,
            note_buf: String::new(),
            in_pres_notes: false,
            slide_notes: String::new(),
            link: None,
            slide_count: 0,
        }
    }

    fn push_text(&mut self, s: &str) {
        if self.in_note_body {
            self.note_buf.push_str(s);
        } else if let Some(t) = self.tables.last_mut().filter(|t| t.in_cell) {
            t.cell_text.push_str(s);
        } else if self.in_pres_notes {
            self.slide_notes.push_str(s);
        } else {
            self.text.push_str(s);
        }
    }

    /// Flush the current inline `text` as a block (paragraph / heading / list item).
    fn flush_paragraph(&mut self) {
        let content = collapse_ws(&self.text);
        self.text.clear();
        // `link` holds a byte index into `text`; clearing `text` invalidates it.
        // A `text:a` left open across a paragraph boundary — which happens for
        // real, because `office:annotation` and `text:note` legally contain
        // their own `text:p` — would otherwise resolve against the NEXT
        // paragraph's bytes, slicing unrelated text as the link label and
        // panicking outright when the stale index lands mid-codepoint.
        self.link = None;
        let level = self.heading_level.take();
        if content.is_empty() {
            return;
        }
        if let Some(lvl) = level {
            let hashes = "#".repeat((lvl as usize).clamp(1, 6));
            self.blocks.push(format!("{hashes} {content}"));
        } else if self.in_list_item && self.list_depth > 0 {
            let indent = "  ".repeat(self.list_depth.saturating_sub(1));
            let ordered = self.ordered_stack.last().copied().unwrap_or(false);
            let marker = if ordered { "1." } else { "-" };
            self.blocks.push(format!("{indent}{marker} {content}"));
        } else {
            self.blocks.push(content);
        }
    }

    fn flush_table(&mut self) {
        // `pop`, not `take`: an unbalanced `</table:table>` is then a no-op
        // rather than clobbering an outer table.
        let Some(mut t) = self.tables.pop() else {
            return;
        };
        t.end_row();
        if t.rows.is_empty() {
            return;
        }
        // A nested table belongs INSIDE its parent's cell, flattened, which is
        // the rule docx already applies (`render_table_inline`). Replacing the
        // parent lost its rows and leaked the remainder into body text.
        if let Some(parent) = self.tables.last_mut() {
            let inline = t
                .rows
                .iter()
                .map(|r| r.join(" | "))
                .collect::<Vec<_>>()
                .join("; ");
            if !inline.is_empty() {
                if !parent.cell_text.is_empty() && !parent.cell_text.ends_with(' ') {
                    parent.cell_text.push(' ');
                }
                parent.cell_text.push_str(&inline);
            }
            return;
        }
        let cols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut md = String::new();
        for (i, row) in t.rows.iter().enumerate() {
            md.push('|');
            for c in 0..cols {
                let cell = row.get(c).map(String::as_str).unwrap_or("");
                md.push(' ');
                md.push_str(&cell.replace('|', "\\|").replace('\n', " "));
                md.push_str(" |");
            }
            md.push('\n');
            if i == 0 {
                md.push('|');
                for _ in 0..cols {
                    md.push_str(" --- |");
                }
                md.push('\n');
            }
        }
        self.blocks.push(md.trim_end().to_string());
    }

    fn start_slide(&mut self) {
        self.slide_count += 1;
        self.blocks.push(format!("## Slide {}", self.slide_count));
    }

    fn end_slide(&mut self) {
        let notes = collapse_ws(&self.slide_notes);
        self.slide_notes.clear();
        if !notes.is_empty() {
            self.blocks.push(format!("**Notes:** {notes}"));
        }
    }
}

/// Walk `content.xml` and produce a markdown document.
/// Map each `<text:list-style>` name to whether it numbers its items.
///
/// ODF puts the marker style in a definition elsewhere in the document, so a
/// `<text:list>` on its own cannot say whether it is ordered. The walker
/// hardcoded `false`, and every numbered list rendered as bullets —
/// odftoolkit_Bullets_and_Numbering.odt has four numbered lists and one
/// bulleted one, and all five came out identical. (#50)
fn ordered_list_styles(content_xml: &str) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    let mut rest = content_xml;
    while let Some(start) = rest.find("<text:list-style") {
        let after = &rest[start..];
        let Some(name_at) = after.find("style:name=\"") else {
            break;
        };
        let name_rest = &after[name_at + 12..];
        let Some(name_end) = name_rest.find('"') else {
            break;
        };
        let name = name_rest[..name_end].to_string();
        let body_end = after.find("</text:list-style>").unwrap_or(after.len());
        let numbered = after[..body_end].contains("<text:list-level-style-number");
        out.insert(name, numbered);
        rest = &after[body_end.min(after.len())..];
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
    }
    out
}

/// Walk `content.xml` into markdown.
///
/// Returns `Err` on malformed XML rather than the prefix parsed so far. A
/// mid-document syntax error means everything after it is unread and the amount
/// lost is unknowable, so returning the prefix with `Ok` made a document that
/// parsed 5% of the way indistinguishable from one that is 5% long. That is the
/// L14 contract — "structurally invalid raises, nothing-to-chunk returns `[]`" —
/// applied to ODF.
pub fn content_to_markdown(
    content_xml: &str,
    kind: OdfKind,
    image_names: &std::collections::HashMap<String, String>,
) -> Result<(String, usize), String> {
    let mut reader = XmlReader::from_str(content_xml);
    reader.config_mut().trim_text(false);
    let mut w = Walker::new();
    w.list_styles = ordered_list_styles(content_xml);
    w.image_names = image_names.clone();
    let mut buf = Vec::new();
    // Open-element depth. quick-xml reports EOF, not an error, when input stops
    // between elements, so a file truncated at an element boundary — the common
    // real case, a partial download or upload — parsed "successfully" and
    // returned its prefix. Only a cut landing mid-markup raised. Counting the
    // depth catches both.
    let mut depth: i64 = 0;

    loop {
        // Read before the match: the scrutinee holds `reader` mutably borrowed
        // for the whole match, so an arm cannot ask it where it got to.
        let pos = reader.buffer_position();
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let name = e.name();
                match local(name.as_ref()) {
                    b"h" => {
                        w.heading_level = attr(&e, b"text:outline-level")
                            .and_then(|s| s.parse::<u8>().ok())
                            .or(Some(1))
                    }
                    // An image contributes nothing to the text otherwise, so a
                    // reader cannot tell one was there. Same `[Image]`
                    // placeholder docx, pptx and Markdown already emit. (#53)
                    b"image" => {
                        // Emit a real Markdown image reference, not a literal
                        // "[Image]". The Markdown chunker strips `[` and `]` as
                        // link syntax, so a bare placeholder arrives as the word
                        // "Image"; `![](…)` is converted to `[Image]` by the
                        // stripper itself and survives intact. It also makes
                        // get_markdown correct, and points at the hashed key the
                        // caller gets back from list_images. (#53)
                        let alt = w.pending_frame_name.take().unwrap_or_default();
                        let href = attr(&e, b"xlink:href").unwrap_or_default();
                        let base = href.rsplit('/').next().unwrap_or(&href).to_string();
                        let name = w.image_names.get(&base).cloned().unwrap_or(base);
                        w.blocks.push(format!("![{}]({})", alt.trim(), name));
                    }
                    b"frame" => {
                        // `draw:name` is the closest thing ODF gives an image to
                        // alt text.
                        w.pending_frame_name = attr(&e, b"draw:name");
                    }
                    b"list" => {
                        w.list_depth += 1;
                        // A nested list usually omits the style name and
                        // continues its parent's, so inherit rather than
                        // falling back to "bullet". (#50)
                        let inherited = w.ordered_stack.last().copied().unwrap_or(false);
                        let ordered = attr(&e, b"text:style-name")
                            .and_then(|n| w.list_styles.get(&n).copied())
                            .unwrap_or(inherited);
                        w.ordered_stack.push(ordered);
                    }
                    b"list-item" => w.in_list_item = true,
                    b"table" => w.tables.push(TableState::default()),
                    b"table-row" => {
                        if let Some(t) = w.tables.last_mut() {
                            t.current_row = Vec::new();
                        }
                    }
                    b"table-cell" | b"covered-table-cell" => {
                        if let Some(t) = w.tables.last_mut() {
                            t.in_cell = true;
                            t.cell_text.clear();
                            t.cell_repeat = cell_repeat(&e);
                        }
                    }
                    b"note-body" => w.in_note_body = true,
                    b"notes" => w.in_pres_notes = true,
                    b"page" if kind == OdfKind::Presentation => w.start_slide(),
                    b"a" => {
                        let href = attr(&e, b"xlink:href").unwrap_or_default();
                        w.link = Some((href, w.text.len()));
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                depth -= 1;
                let name = e.name();
                match local(name.as_ref()) {
                    b"p" | b"h" => w.flush_paragraph(),
                    b"a" => {
                        if let Some((href, start)) = w.link.take() {
                            if start <= w.text.len()
                                && w.text.is_char_boundary(start)
                                && !href.is_empty()
                            {
                                let label = w.text[start..].to_string();
                                w.text.truncate(start);
                                w.text.push_str(&format!("[{label}]({href})"));
                            }
                        }
                    }
                    b"list" => {
                        w.list_depth = w.list_depth.saturating_sub(1);
                        w.ordered_stack.pop();
                        if w.list_depth == 0 {
                            w.in_list_item = false;
                        }
                    }
                    b"list-item" => w.in_list_item = w.list_depth > 0,
                    b"table-cell" | b"covered-table-cell" => {
                        if let Some(t) = w.tables.last_mut() {
                            let cell = collapse_ws(&t.cell_text);
                            let rep = t.cell_repeat;
                            t.push_cell(cell, rep);
                            t.in_cell = false;
                            t.cell_text.clear();
                            t.cell_repeat = 1;
                        }
                    }
                    b"table-row" => {
                        if let Some(t) = w.tables.last_mut() {
                            t.end_row();
                        }
                    }
                    b"table" => w.flush_table(),
                    b"note-body" => {
                        w.in_note_body = false;
                        let note = collapse_ws(&w.note_buf);
                        w.note_buf.clear();
                        if !note.is_empty() {
                            w.notes.push(note);
                        }
                    }
                    b"notes" => w.in_pres_notes = false,
                    b"page" if kind == OdfKind::Presentation => w.end_slide(),
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"tab" | b"s" => w.push_text(" "),
                b"line-break" => w.push_text("\n"),
                // An empty ODF cell is written `<table:table-cell/>`, which
                // quick-xml reports as Empty, not Start+End — so it never
                // reached the cell arms and no cell was pushed at all. Every
                // later cell in the row then shifted left into the wrong
                // column. Measured on odftoolkit_Presentation2.odp, where a
                // 3-column table rendered as one column.
                b"table-cell" | b"covered-table-cell" => {
                    if let Some(t) = w.tables.last_mut() {
                        let rep = cell_repeat(&e);
                        t.push_cell(String::new(), rep);
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let decoded = t.decode().unwrap_or_default();
                if !decoded.is_empty() {
                    let txt = decode_entities(&decoded);
                    w.push_text(&txt);
                }
            }
            Ok(Event::Eof) => {
                if depth > 0 {
                    return Err(format!(
                        "ODF content.xml ends with {depth} unclosed element(s) \
                         at byte {pos}: the document is truncated"
                    ));
                }
                break;
            }
            // Was `break`, which kept the prefix and reported success.
            Err(e) => return Err(format!("ODF content.xml is malformed at byte {pos}: {e}")),
            _ => {}
        }
        buf.clear();
    }

    // Trailing footnotes.
    if !w.notes.is_empty() {
        w.blocks.push("## Notes".to_string());
        for (i, note) in w.notes.iter().enumerate() {
            w.blocks.push(format!("{}. {note}", i + 1));
        }
    }

    let md = w.blocks.join("\n\n");
    Ok((md.trim().to_string(), w.slide_count))
}
