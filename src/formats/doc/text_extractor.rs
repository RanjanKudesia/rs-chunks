use super::paragraph_props::ParagraphProp;
use super::piece_table::ReconstructedText;
use super::stylesheet::{StyleKind, StyleSheet};
use super::tables::{CellMark, DocTable, TableShape};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParagraphType {
    Heading(u8),
    Normal,
    Table,
    ListItem,
    PageBreak,
}

#[derive(Debug, Clone)]
pub struct DocParagraph {
    pub content: String,
    pub paragraph_type: ParagraphType,
    pub heading_level: Option<u8>,
    /// 0-based ordinal of the page (`.doc`) or slide (`.ppt`) this paragraph
    /// belongs to, or `None` when the source carries no such structure.
    ///
    /// `.doc` counts hard page breaks (`\x0C`); Word does not store soft
    /// pagination, so this is every page boundary the file actually declares.
    /// `.ppt` uses the true slide ordinal — *not* a count of the emitted
    /// separators, which skip text-free slides and would misnumber exactly the
    /// way TECH_DEBT #19 did.
    pub page_index: Option<usize>,
    /// 0-based list nesting depth, from `sprmPIlvl`. Set only on a `ListItem`
    /// (TECH_DEBT #12).
    pub list_level: Option<u8>,
    /// Row/column/cell counts of the table this paragraph renders. Set only on
    /// a `Table` paragraph, which holds a whole table (TECH_DEBT #12).
    pub table: Option<TableShape>,
}

impl DocParagraph {
    /// A plain paragraph carrying no `.doc`-specific structure — the shape
    /// `.ppt` and the markdown image interleaver build.
    pub fn plain(
        content: String,
        paragraph_type: ParagraphType,
        page_index: Option<usize>,
    ) -> Self {
        let heading_level = match paragraph_type {
            ParagraphType::Heading(level) => Some(level),
            _ => None,
        };
        DocParagraph {
            content,
            paragraph_type,
            heading_level,
            page_index,
            list_level: None,
            table: None,
        }
    }
}

pub(super) fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
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

fn looks_like_fallback_heading(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.is_empty() || words.len() > 10 {
        return false;
    }
    // A one-character paragraph is a drop cap, not a heading. Word puts the
    // first letter of a section in its own paragraph — sample.doc's "T" ahead
    // of "HIS document is a template…" — and title-case matched it. Harmless
    // while it only produced a stray heading chunk; once headings carry a
    // breadcrumb it made the next five chunks claim to live in a section
    // called "T" (#12).
    if trimmed.chars().count() < 2 {
        return false;
    }
    if trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?') {
        return false;
    }

    let has_alpha = trimmed.chars().any(|c| c.is_alphabetic());
    if !has_alpha {
        return false;
    }

    let all_caps = trimmed
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(|c| c.is_uppercase());

    let title_case = words
        .iter()
        .all(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false));

    all_caps || title_case
}

/// One Word paragraph: its text and the mark that ended it.
///
/// Word paragraphs end at `\r` *or* at a cell mark `\x07` — in the binary
/// format a cell mark is a paragraph mark, and the PAPX FKPs are indexed in
/// these units. Splitting on `\r` alone made every cell of a row a single unit
/// and left the row structure unrecoverable.
struct Unit<'a> {
    text: &'a str,
    is_cell_mark: bool,
}

fn split_units(text: &str) -> Vec<Unit<'_>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == '\r' || ch == '\x07' {
            out.push(Unit {
                text: &text[start..idx],
                is_cell_mark: ch == '\x07',
            });
            start = idx + ch.len_utf8();
        }
    }
    out.push(Unit {
        text: &text[start..],
        is_cell_mark: false,
    });
    out
}

fn is_in_table(unit: &Unit<'_>, prop: Option<&ParagraphProp>) -> bool {
    unit.is_cell_mark || prop.map(|p| p.f_in_table).unwrap_or(false)
}

pub fn extract_paragraphs(
    story: &ReconstructedText,
    props: &[Option<ParagraphProp>],
    stylesheet: &StyleSheet,
) -> Vec<DocParagraph> {
    extract_paragraphs_indexed(story, props, stylesheet)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

/// Like [`extract_paragraphs`] but each paragraph carries the index of the raw
/// Word paragraph it came from (empty paragraphs are skipped, so output indices
/// are sparse). Used by the image extractor to anchor inline pictures — whose
/// position is known as a raw paragraph ordinal — to the surviving stream.
pub fn extract_paragraphs_indexed(
    story: &ReconstructedText,
    props: &[Option<ParagraphProp>],
    stylesheet: &StyleSheet,
) -> Vec<(usize, DocParagraph)> {
    let units = split_units(&story.text);
    let mut out = Vec::new();
    // Word stores hard page breaks only; soft pagination is recomputed by the
    // renderer and is not in the file. So a document that declares no breaks
    // carries no page information at all — and reporting "page 1" for every
    // chunk of a 900-chunk document would claim pagination we do not have.
    // In that case `page_index` stays `None` and `page_number` stays null,
    // which is what shipped and is the honest answer (TECH_DEBT #11).
    let declares_pages = units.iter().any(|u| u.text.starts_with('\x0C'));
    let mut page_index = 0usize;

    let mut idx = 0usize;
    while idx < units.len() {
        let unit = &units[idx];
        let prop = props.get(idx).and_then(Option::as_ref);

        if unit.text.starts_with('\x0C') {
            out.push((
                idx,
                DocParagraph::plain(String::new(), ParagraphType::PageBreak, Some(page_index)),
            ));
            page_index += 1;
            idx += 1;
            continue;
        }

        if is_in_table(unit, prop) {
            let (table, consumed) = collect_table(&units, props, idx);
            idx += consumed;
            if !table.is_empty() {
                let shape = table.shape();
                out.push((
                    idx - consumed,
                    DocParagraph {
                        content: table.to_markdown(),
                        paragraph_type: ParagraphType::Table,
                        heading_level: None,
                        page_index: declares_pages.then_some(page_index),
                        list_level: None,
                        table: Some(shape),
                    },
                ));
            }
            continue;
        }

        if let Some(paragraph) = classify_paragraph(unit.text, prop, stylesheet) {
            out.push((
                idx,
                DocParagraph {
                    page_index: declares_pages.then_some(page_index),
                    ..paragraph
                },
            ));
        }
        idx += 1;
    }

    out
}

/// Consume the run of in-table paragraphs starting at `start`, returning the
/// assembled table and how many units it spanned.
fn collect_table(
    units: &[Unit<'_>],
    props: &[Option<ParagraphProp>],
    start: usize,
) -> (DocTable, usize) {
    let mut cells: Vec<(&str, CellMark)> = Vec::new();
    let mut idx = start;
    while idx < units.len() {
        let unit = &units[idx];
        let prop = props.get(idx).and_then(Option::as_ref);
        if !is_in_table(unit, prop) {
            break;
        }
        let mark = if !unit.is_cell_mark {
            CellMark::Line
        } else if prop.map(|p| p.f_tt_p).unwrap_or(false) {
            CellMark::EndOfRow
        } else {
            CellMark::EndOfCell
        };
        cells.push((unit.text, mark));
        idx += 1;
    }
    // Without paragraph properties there is no row mark to cut on — every cell
    // mark reads as `EndOfCell`, which would collapse the whole table into one
    // row. Fall back to the last-resort signal: an empty cell terminates a row,
    // which is how Word writes the row mark's own (text-free) paragraph.
    if !cells.iter().any(|(_, m)| *m == CellMark::EndOfRow) {
        for entry in cells.iter_mut() {
            if entry.0.trim().is_empty() && entry.1 == CellMark::EndOfCell {
                entry.1 = CellMark::EndOfRow;
            }
        }
    }
    (DocTable::from_units(cells), idx - start)
}

/// Classify a non-table Word paragraph. Returns `None` for a paragraph with no
/// content to emit.
fn classify_paragraph(
    raw: &str,
    prop: Option<&ParagraphProp>,
    stylesheet: &StyleSheet,
) -> Option<DocParagraph> {
    let style = prop.map(|p| stylesheet.kind(p.istd as usize));

    let mut paragraph_type = ParagraphType::Normal;
    let mut heading_level = None;
    let mut list_level = None;

    if let Some(p) = prop {
        if p.ilfo > 0 || matches!(style, Some(StyleKind::ListParagraph)) {
            paragraph_type = ParagraphType::ListItem;
            list_level = Some(p.ilvl);
        }
    }

    if matches!(paragraph_type, ParagraphType::Normal) {
        if let Some(StyleKind::Heading(level)) = style {
            paragraph_type = ParagraphType::Heading(level);
            heading_level = Some(level);
        }
    }

    let content = collapse_whitespace(&raw.replace(['\x07', '\x0C'], " "));

    if matches!(paragraph_type, ParagraphType::Normal) && content.is_empty() {
        return None;
    }

    // Not every `.doc` uses the built-in heading styles — sample.doc marks
    // "Appendix" and "Acknowledgment" with a custom "Reference Head" style —
    // so capitalisation remains the fallback for an unrecognised style.
    if matches!(paragraph_type, ParagraphType::Normal) && looks_like_fallback_heading(&content) {
        paragraph_type = ParagraphType::Heading(2);
        heading_level = Some(2);
    }

    Some(DocParagraph {
        content,
        paragraph_type,
        heading_level,
        page_index: None,
        list_level,
        table: None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::piece_table::{reconstruct_from_pieces, Piece};
    use super::*;

    /// Build a story from a compressed (cp1252) byte run, plus its properties.
    fn story_of(text: &str) -> ReconstructedText {
        let bytes: Vec<u8> = text.chars().map(|c| c as u8).collect();
        let pieces = vec![Piece {
            cp_start: 0,
            cp_end: bytes.len() as u32,
            fc: 0,
            compressed: true,
        }];
        reconstruct_from_pieces(&bytes, &pieces)
    }

    fn empty_styles() -> StyleSheet {
        StyleSheet { styles: Vec::new() }
    }

    fn heading_styles() -> StyleSheet {
        StyleSheet {
            styles: vec![
                StyleKind::Normal,
                StyleKind::Heading(1),
                StyleKind::Heading(2),
                StyleKind::Heading(3),
            ],
        }
    }

    fn prop(istd: u16) -> Option<ParagraphProp> {
        Some(ParagraphProp {
            istd,
            ..Default::default()
        })
    }

    /// Heading level now comes from the paragraph's style, not from guessing at
    /// its capitalisation — which reported level 2 for every heading in every
    /// `.doc` because the stylesheet never parsed (TECH_DEBT #12, #10).
    #[test]
    fn heading_level_comes_from_the_style() {
        let story = story_of("Chapter One\rSection Two\rbody text here.\r");
        let props = vec![prop(1), prop(2), prop(0), None];
        let paragraphs = extract_paragraphs(&story, &props, &heading_styles());
        let kinds: Vec<_> = paragraphs
            .iter()
            .map(|p| p.paragraph_type.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                ParagraphType::Heading(1),
                ParagraphType::Heading(2),
                ParagraphType::Normal
            ]
        );
        assert_eq!(paragraphs[0].heading_level, Some(1));
    }

    #[test]
    fn a_list_paragraph_reports_its_nesting_depth() {
        let story = story_of("Top level\rNested\rDeeper\r");
        let props = vec![
            Some(ParagraphProp {
                ilfo: 1,
                ilvl: 0,
                ..Default::default()
            }),
            Some(ParagraphProp {
                ilfo: 1,
                ilvl: 1,
                ..Default::default()
            }),
            Some(ParagraphProp {
                ilfo: 1,
                ilvl: 2,
                ..Default::default()
            }),
            None,
        ];
        let paragraphs = extract_paragraphs(&story, &props, &empty_styles());
        assert!(paragraphs
            .iter()
            .all(|p| p.paragraph_type == ParagraphType::ListItem));
        assert_eq!(
            paragraphs.iter().map(|p| p.list_level).collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2)]
        );
    }

    /// The row mark is a cell mark whose paragraph carries `sprmPFTtp`. Without
    /// it every cell of the table lands in one row.
    #[test]
    fn a_table_is_rebuilt_into_rows_and_columns() {
        let story = story_of("a\x07b\x07\x07c\x07d\x07\x07after\r");
        let cell = ParagraphProp {
            f_in_table: true,
            ..Default::default()
        };
        let row_end = ParagraphProp {
            f_in_table: true,
            f_tt_p: true,
            ..Default::default()
        };
        let props = vec![
            Some(cell.clone()),
            Some(cell.clone()),
            Some(row_end.clone()),
            Some(cell.clone()),
            Some(cell),
            Some(row_end),
            None,
            None,
        ];
        let paragraphs = extract_paragraphs(&story, &props, &empty_styles());
        assert_eq!(
            paragraphs.len(),
            2,
            "one table paragraph, then the body text"
        );
        let table = &paragraphs[0];
        assert_eq!(table.paragraph_type, ParagraphType::Table);
        assert_eq!(
            table.table,
            Some(TableShape {
                rows: 2,
                columns: 2,
                cells: 4
            })
        );
        assert_eq!(table.content, "| a | b |\n| --- | --- |\n| c | d |");
        assert_eq!(paragraphs[1].content, "after");
    }

    /// Properties are absent when a paragraph takes its style's defaults, so
    /// the cell marks alone still have to yield a table.
    #[test]
    fn a_table_without_properties_falls_back_to_the_empty_row_mark() {
        let story = story_of("a\x07b\x07\x07c\x07d\x07\x07");
        let props: Vec<Option<ParagraphProp>> = vec![None; 7];
        let paragraphs = extract_paragraphs(&story, &props, &empty_styles());
        assert_eq!(paragraphs.len(), 1);
        assert_eq!(
            paragraphs[0].table,
            Some(TableShape {
                rows: 2,
                columns: 2,
                cells: 4
            })
        );
    }

    /// Word's drop cap puts a section's first letter in its own paragraph.
    /// Title-case matched it, so "T" became a heading and — once headings
    /// carried a breadcrumb — the body that followed claimed to be inside a
    /// section named "T".
    #[test]
    fn a_drop_cap_is_not_a_heading() {
        let story = story_of("INTRODUCTION\rT\rHIS document is a template.\r");
        let props: Vec<Option<ParagraphProp>> = vec![None; 4];
        let paragraphs = extract_paragraphs(&story, &props, &empty_styles());
        assert_eq!(paragraphs[0].paragraph_type, ParagraphType::Heading(2));
        assert_eq!(
            paragraphs[1].paragraph_type,
            ParagraphType::Normal,
            "a one-character paragraph is a drop cap"
        );
    }

    #[test]
    fn a_page_break_still_advances_the_page_ordinal() {
        let story = story_of("first page body\r\x0C\rsecond page body\r");
        let props: Vec<Option<ParagraphProp>> = vec![None; 4];
        let paragraphs = extract_paragraphs(&story, &props, &empty_styles());
        let pages: Vec<_> = paragraphs.iter().map(|p| p.page_index).collect();
        assert_eq!(pages, vec![Some(0), Some(0), Some(1)]);
    }
}
