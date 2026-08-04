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

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    in_cell: bool,
    cell_text: String,
}

struct Walker {
    blocks: Vec<String>,
    /// Inline text of the current paragraph/heading/cell.
    text: String,
    heading_level: Option<u8>,
    list_depth: usize,
    in_list_item: bool,
    ordered_stack: Vec<bool>,
    table: Option<TableState>,
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

fn attr<'a>(e: &'a quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key || local(a.key.as_ref()) == local(key) {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
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
            table: None,
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
        } else if let Some(t) = self.table.as_mut().filter(|t| t.in_cell) {
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
        let Some(mut t) = self.table.take() else { return };
        if !t.current_row.is_empty() {
            t.rows.push(std::mem::take(&mut t.current_row));
        }
        if t.rows.is_empty() {
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
pub fn content_to_markdown(content_xml: &str, kind: OdfKind) -> (String, usize) {
    let mut reader = XmlReader::from_str(content_xml);
    reader.config_mut().trim_text(false);
    let mut w = Walker::new();
    let mut buf = Vec::new();

    loop {
        // Entity references arrive as their own event; fold them back into text.
        let mut spill = String::new();
        match read_event_folding_entities!(reader, &mut buf, &mut spill) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                match local(name.as_ref()) {
                    b"h" => w.heading_level = attr(&e, b"text:outline-level")
                        .and_then(|s| s.parse::<u8>().ok())
                        .or(Some(1)),
                    b"list" => {
                        w.list_depth += 1;
                        // Heuristic: numbered if a number style is referenced; we
                        // default to bullet and let list-item text carry meaning.
                        w.ordered_stack.push(false);
                    }
                    b"list-item" => w.in_list_item = true,
                    b"table" => w.table = Some(TableState::default()),
                    b"table-row" => {
                        if let Some(t) = w.table.as_mut() {
                            t.current_row = Vec::new();
                        }
                    }
                    b"table-cell" => {
                        if let Some(t) = w.table.as_mut() {
                            t.in_cell = true;
                            t.cell_text.clear();
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
                let name = e.name();
                match local(name.as_ref()) {
                    b"p" | b"h" => w.flush_paragraph(),
                    b"a" => {
                        if let Some((href, start)) = w.link.take() {
                            if start <= w.text.len() && !href.is_empty() {
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
                    b"table-cell" => {
                        if let Some(t) = w.table.as_mut() {
                            let cell = collapse_ws(&t.cell_text);
                            t.current_row.push(cell);
                            t.in_cell = false;
                            t.cell_text.clear();
                        }
                    }
                    b"table-row" => {
                        if let Some(t) = w.table.as_mut() {
                            if !t.current_row.is_empty() {
                                t.rows.push(std::mem::take(&mut t.current_row));
                            }
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
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let decoded = t.decode().unwrap_or_default();
                if !decoded.is_empty() {
                    let txt = decode_entities(&decoded);
                    w.push_text(&txt);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // Malformed XML: stop, keep what we have.
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
    (md.trim().to_string(), w.slide_count)
}
