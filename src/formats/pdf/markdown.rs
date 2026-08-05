//! Blocks → markdown.
//!
//! Emphasis is deliberately sparing. A paper sets every variable in italic, so
//! honouring italic run-by-run turns `the value of n` into `the value of *n*`
//! across the whole document — markup that describes the typesetting rather than
//! the meaning. Bold is not used that way, so short bold runs survive inline
//! while italic only survives when it covers a whole block (a note, a caption).

use super::blocks::Block;
use super::lines::Span;

/// Pages are joined by a rule, which is also what the markdown chunker's
/// `page_aware` mode splits on.
pub(crate) const PAGE_SEPARATOR: &str = "\n\n---\n\n";

/// Shorter bold runs than this are a bold variable in a formula, not emphasis.
const MIN_BOLD_RUN: usize = 3;

/// One thing to place on a page, ordered by how far down the page it sits.
pub(crate) enum Item {
    Block(Block),
    Image(String),
}

/// Render one page. `items` must already be in reading order.
pub(crate) fn page(items: &[Item]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        let text = match item {
            Item::Image(name) => format!("![]({name})"),
            Item::Block(block) => render(block),
        };
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    parts.join("\n\n")
}

fn render(block: &Block) -> String {
    match block {
        Block::Heading { level, spans } => {
            let text = plain(spans);
            if text.trim().is_empty() {
                String::new()
            } else {
                format!("{} {}", "#".repeat(*level as usize), text.trim())
            }
        }
        Block::Paragraph { spans } => emphasised(spans),
        Block::ListItem { marker, spans } => {
            let text = emphasised(spans);
            if text.trim().is_empty() {
                String::new()
            } else {
                format!("{marker} {}", text.trim())
            }
        }
        Block::Table { rows } => table(rows),
    }
}

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect::<String>()
}

/// Wrap bold runs inline; wrap the whole block in italics only if all of it is.
fn emphasised(spans: &[Span]) -> String {
    let text = plain(spans);
    if text.trim().is_empty() {
        return String::new();
    }
    let all_italic = spans
        .iter()
        .all(|s| s.italic || s.text.trim().is_empty())
        && spans.iter().any(|s| s.italic);

    let mut out = String::with_capacity(text.len());
    for span in spans {
        let significant = span.text.chars().filter(|c| !c.is_whitespace()).count();
        let mut piece = if span.bold && significant >= MIN_BOLD_RUN {
            wrap(&span.text, "**")
        } else {
            span.text.clone()
        };
        // The link target lives in the page's annotations, not in the text, so
        // this is the only place it can be written down.
        if let Some(uri) = &span.link {
            if significant > 0 {
                piece = link(&piece, uri);
            }
        }
        out.push_str(&piece);
    }
    let out = collapse(&out);
    if all_italic {
        wrap(&out, "*")
    } else {
        out
    }
}

/// Put the markers tight against the text. Markdown ignores emphasis whose
/// marker touches whitespace, so `** bold **` renders as literal asterisks —
/// the same trap the RTF emphasis work hit.
fn wrap(text: &str, marker: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return text.to_string();
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let tail = &text[text.trim_end().len()..];
    format!("{lead}{marker}{trimmed}{marker}{tail}")
}

/// `[text](uri)`, with the brackets kept off the surrounding whitespace so the
/// link text is exactly the words the annotation covers.
fn link(text: &str, uri: &str) -> String {
    let trimmed = text.trim();
    let lead = &text[..text.len() - text.trim_start().len()];
    let tail = &text[text.trim_end().len()..];
    format!("{lead}[{trimmed}]({}){tail}", uri.replace(')', "%29"))
}

fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

fn table(rows: &[Vec<String>]) -> String {
    let Some(width) = rows.iter().map(Vec::len).max() else { return String::new() };
    if width == 0 {
        return String::new();
    }
    let line = |cells: &Vec<String>| {
        let mut padded: Vec<&str> = cells.iter().map(|c| c.trim()).collect();
        padded.resize(width, "");
        format!("| {} |", padded.join(" | "))
    };
    let mut out = vec![line(&rows[0]), format!("|{}", "---|".repeat(width))];
    out.extend(rows[1..].iter().map(line));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, bold: bool, italic: bool) -> Span {
        Span { text: text.into(), bold, italic, link: None }
    }

    #[test]
    fn a_link_annotation_becomes_a_markdown_link() {
        let mut linked = span("Semeval-2017 task 1", false, false);
        linked.link = Some(std::rc::Rc::from("https://doi.org/10.18653/v1/S17-2001"));
        let spans = vec![span("See ", false, false), linked, span(" for details", false, false)];
        assert_eq!(
            emphasised(&spans),
            "See [Semeval-2017 task 1](https://doi.org/10.18653/v1/S17-2001) for details"
        );
    }

    #[test]
    fn a_bold_run_is_wrapped_and_the_markers_stay_off_the_whitespace() {
        let spans = vec![span("Encoder: ", true, false), span("the encoder is a stack", false, false)];
        assert_eq!(
            emphasised(&spans),
            "**Encoder:** the encoder is a stack"
        );
    }

    #[test]
    fn a_bold_variable_is_left_alone() {
        let spans = vec![span("the vector ", false, false), span("x", true, false), span(" is fixed", false, false)];
        assert_eq!(emphasised(&spans), "the vector x is fixed");
    }

    #[test]
    fn italic_survives_only_when_it_covers_the_block() {
        let inline = vec![span("the value of ", false, false), span("n", false, true), span(" grows", false, false)];
        assert_eq!(emphasised(&inline), "the value of n grows");

        let whole = vec![span("Equal contribution. Listing order is random.", false, true)];
        assert_eq!(emphasised(&whole), "*Equal contribution. Listing order is random.*");
    }

    #[test]
    fn a_table_gets_a_header_rule_and_square_rows() {
        let rows = vec![
            vec!["Layer".into(), "Ops".into()],
            vec!["Recurrent".into()],
        ];
        assert_eq!(table(&rows), "| Layer | Ops |\n|---|---|\n| Recurrent |  |");
    }

    #[test]
    fn a_heading_takes_one_hash_per_level() {
        let block = Block::Heading { level: 3, spans: vec![span("  Positional Encoding ", false, false)] };
        assert_eq!(render(&block), "### Positional Encoding");
    }

    #[test]
    fn an_image_is_placed_as_a_reference_of_its_own() {
        let items = vec![
            Item::Block(Block::Paragraph { spans: vec![span("above", false, false)] }),
            Item::Image("image_p3_1.png".into()),
        ];
        assert_eq!(page(&items), "above\n\n![](image_p3_1.png)");
    }
}
