use super::paragraph_props::ParagraphProp;
use super::piece_table::ReconstructedText;
use super::stylesheet::{StyleKind, StyleSheet};

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

pub fn extract_paragraphs(
    reconstructed: &ReconstructedText,
    props: &[ParagraphProp],
    stylesheet: &StyleSheet,
) -> Vec<DocParagraph> {
    extract_paragraphs_indexed(reconstructed, props, stylesheet)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

/// Like [`extract_paragraphs`] but each paragraph carries the index of the
/// raw `\r`-separated paragraph it came from (empty paragraphs are skipped,
/// so output indices are sparse). Used by the image extractor to anchor
/// inline pictures — whose position is known as a raw paragraph ordinal —
/// to the surviving paragraph stream.
pub fn extract_paragraphs_indexed(
    reconstructed: &ReconstructedText,
    props: &[ParagraphProp],
    stylesheet: &StyleSheet,
) -> Vec<(usize, DocParagraph)> {
    let raw_paragraphs: Vec<&str> = reconstructed.text.split('\r').collect();
    let mut out = Vec::new();
    // Word stores hard page breaks only; soft pagination is recomputed by the
    // renderer and is not in the file. So a document that declares no breaks
    // carries no page information at all — and reporting "page 1" for every
    // chunk of a 900-chunk document would claim pagination we do not have.
    // In that case `page_index` stays `None` and `page_number` stays null,
    // which is what shipped and is the honest answer (TECH_DEBT #11).
    let declares_pages = raw_paragraphs
        .iter()
        .any(|raw| *raw == "\x0C" || raw.starts_with('\x0C'));
    let mut page_index = 0usize;

    for (idx, raw) in raw_paragraphs.iter().enumerate() {
        let prop = props.get(idx);

        if raw == &"\x0C" || raw.starts_with('\x0C') {
            out.push((
                idx,
                DocParagraph {
                    content: String::new(),
                    paragraph_type: ParagraphType::PageBreak,
                    heading_level: None,
                    page_index: Some(page_index),
                },
            ));
            page_index += 1;
            continue;
        }

        let mut paragraph_type = ParagraphType::Normal;
        let mut heading_level = None;

        let mut content = if raw.contains('\x07') {
            paragraph_type = ParagraphType::Table;
            raw.split('\x07')
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join(" | ")
        } else {
            raw.to_string()
        };

        if matches!(paragraph_type, ParagraphType::Normal)
            && prop.map(|p| p.f_in_table).unwrap_or(false)
        {
            paragraph_type = ParagraphType::Table;
            content = content.replace('\x07', " ");
        }

        if matches!(paragraph_type, ParagraphType::Normal)
            && prop
                .map(|p| p.ilfo > 0 || matches!(stylesheet.kind(p.istd as usize), StyleKind::ListParagraph))
                .unwrap_or(false)
        {
            paragraph_type = ParagraphType::ListItem;
        }

        if matches!(paragraph_type, ParagraphType::Normal) {
            if let Some(p) = prop {
                if let StyleKind::Heading(level) = stylesheet.kind(p.istd as usize) {
                    paragraph_type = ParagraphType::Heading(level);
                    heading_level = Some(level);
                }
            }
        }

        content = content.replace('\x07', " ");

        if !matches!(paragraph_type, ParagraphType::PageBreak) {
            content = content.replace('\x0C', " ");
        }

        content = collapse_whitespace(&content);

        if matches!(paragraph_type, ParagraphType::Normal) && content.is_empty() {
            continue;
        }

        if matches!(paragraph_type, ParagraphType::Normal) && looks_like_fallback_heading(&content) {
            paragraph_type = ParagraphType::Heading(2);
            heading_level = Some(2);
        }

        out.push((
            idx,
            DocParagraph {
                content,
                paragraph_type,
                heading_level,
                page_index: declares_pages.then_some(page_index),
            },
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_print_paragraphs() {
        let path = match std::env::var("DOC_TEST_FILE") {
            Ok(p) => p,
            Err(_) => return,
        };
        let bytes = std::fs::read(&path).unwrap();
        let word_doc = crate::formats::doc::cfb_reader::read_word_document_stream(&bytes).unwrap();
        let fib = crate::formats::doc::fib::parse_fib(&word_doc).unwrap();
        let table = crate::formats::doc::cfb_reader::read_table_stream(&bytes, fib.f_which_tbl_stm).unwrap();
        let reconstructed = crate::formats::doc::piece_table::parse_piece_table(
            &word_doc,
            &table,
            fib.fc_clx,
            fib.lcb_clx,
            fib.ccp_text,
        )
        .unwrap();
        let stylesheet = crate::formats::doc::stylesheet::parse_stylesheet(
            &table,
            fib.fc_stshf,
            fib.lcb_stshf,
        )
        .unwrap();
        let props = crate::formats::doc::paragraph_props::parse_paragraph_props(
            &word_doc,
            &table,
            fib.fc_plcf_papx,
            fib.lcb_plcf_papx,
        )
        .unwrap();
        let paragraphs = extract_paragraphs(&reconstructed, &props, &stylesheet);
        for (i, p) in paragraphs.iter().enumerate() {
            println!(
                "[{i}] {:?} | {}",
                p.paragraph_type,
                &p.content[..p.content.len().min(80)]
            );
        }
    }
}
