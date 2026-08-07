//! Rendering parsed slides to markdown text.

use std::collections::HashMap;

use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::md_blocks::{BlockKind, SlideMarkdownContent};
use super::md_rels::image_hash_name;


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

pub(super) fn slide_to_markdown(slide_num: usize, slide: &SlideMarkdownContent) -> String {
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

pub(super) fn slide_to_markdown_with_images(
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
