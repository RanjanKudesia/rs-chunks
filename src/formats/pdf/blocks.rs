//! Lines → blocks: paragraphs, headings, list items and tables.
//!
//! Heading levels come from *size*, ranked across the whole document rather than
//! guessed per page — a 14 pt line is a section head in a paper set in 10 pt and
//! body text in a report set in 14 pt, and only the document knows which.
//!
//! Tables are recovered from the cell gaps inside a line. A table row spans the
//! full width of its column, so the region cut in [`super::regions`] leaves it
//! whole; a run of such lines whose gaps line up is a table.

use super::lines::{Line, Span};
#[cfg(test)]
use super::lines::Segment;

/// A line must be this much larger than body text to count as a heading. Small
/// enough to catch a 11 pt head over 10 pt body, large enough that the optical
/// size differences inside a paragraph do not qualify.
const HEADING_RATIO: f32 = 1.12;

/// Sizes closer than this are the same size, before and after rounding.
const SIZE_EPSILON: f32 = 0.6;

/// Markdown has six heading levels; deeper ranks all land on the sixth.
const MAX_LEVEL: u8 = 6;

/// A line shorter than its block's measure by this many ems ends a paragraph.
const SHORT_LINE_EMS: f32 = 2.5;

/// Indentation this deep, in ems, starts a new paragraph.
const INDENT_EMS: f32 = 1.2;

/// Two segments whose left edges are within this many ems share a column.
const COLUMN_TOLERANCE_EMS: f32 = 1.5;

/// A bold line must fall this many ems short of the measure to be a heading.
const HEADING_SLACK_EMS: f32 = 4.0;

#[derive(Debug)]
pub(crate) enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph { spans: Vec<Span> },
    ListItem { marker: String, spans: Vec<Span> },
    Table { rows: Vec<Vec<String>> },
}

/// The document's type sizes: what body text is set in, and which larger sizes
/// are used often enough to be structure rather than accident.
pub(crate) struct Style {
    body: f32,
    headings: Vec<f32>,
}

impl Style {
    pub fn of(lines: &[Line]) -> Style {
        let mut weights: Vec<(f32, usize)> = Vec::new();
        for line in lines {
            let n = line.text.chars().filter(|c| !c.is_whitespace()).count();
            if n == 0 || line.size <= 0.0 {
                continue;
            }
            match weights.iter_mut().find(|(s, _)| (*s - line.size).abs() < SIZE_EPSILON) {
                Some((_, count)) => *count += n,
                None => weights.push((line.size, n)),
            }
        }
        let body = weights
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(size, _)| *size)
            .unwrap_or(0.0);

        // A size used for a handful of characters in the whole document is a
        // formula or a logo, not a heading level.
        let total: usize = weights.iter().map(|(_, c)| *c).sum();
        let floor = (total / 500).max(8);
        let mut headings: Vec<f32> = weights
            .into_iter()
            .filter(|(size, count)| *size > body * HEADING_RATIO && *count >= floor)
            .map(|(size, _)| size)
            .collect();
        headings.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        Style { body, headings }
    }

    pub fn body(&self) -> f32 {
        self.body
    }

    /// How many size-based heading levels the document has.
    pub fn levels(&self) -> usize {
        self.headings.len()
    }

    /// The heading level a size maps to, or `None` for body text.
    pub fn level(&self, size: f32) -> Option<u8> {
        let rank = self.headings.iter().position(|h| (h - size).abs() < SIZE_EPSILON)?;
        Some((rank as u8 + 1).min(MAX_LEVEL))
    }
}

/// Turn one region's lines into blocks.
pub(crate) fn build(lines: &[Line], style: &Style) -> Vec<Block> {
    // The measure is the region's, not the running block's: a paragraph ends
    // where a line falls short of the *column*, and its own first line may
    // already be that short line.
    let measure = lines.iter().map(|l| l.right).fold(f32::NEG_INFINITY, f32::max);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(end) = table_end(lines, i) {
            out.push(Block::Table { rows: rows_of(&lines[i..end]) });
            i = end;
            continue;
        }
        let end = prose_end(lines, i, style, measure);
        out.push(prose(&lines[i..end], style, measure));
        i = end;
    }
    out
}

/// Where the table starting at `start` ends, or `None` if there is not one.
fn table_end(lines: &[Line], start: usize) -> Option<usize> {
    let mut end = start;
    while end < lines.len() && lines[end].segments.len() >= 2 {
        end += 1;
    }
    // One line of aligned gaps is a wide-set heading or a caption with a page
    // number, not a table.
    if end - start < 2 {
        return None;
    }
    // The gaps must line up: a real table has at least two columns that recur.
    if columns_of(&lines[start..end]).len() < 2 {
        return None;
    }
    Some(end)
}

/// The x positions the run's cells align on.
fn columns_of(lines: &[Line]) -> Vec<f32> {
    let tolerance = COLUMN_TOLERANCE_EMS * median_size(lines);
    let mut clusters: Vec<(f32, usize)> = Vec::new();
    for segment in lines.iter().flat_map(|l| l.segments.iter()) {
        match clusters.iter_mut().find(|(x, _)| (*x - segment.left).abs() <= tolerance) {
            Some((_, count)) => *count += 1,
            None => clusters.push((segment.left, 1)),
        }
    }
    // A column that appears in only one row is a stray indent.
    clusters.retain(|(_, count)| *count >= 2);
    clusters.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    clusters.into_iter().map(|(x, _)| x).collect()
}

fn rows_of(lines: &[Line]) -> Vec<Vec<String>> {
    let columns = columns_of(lines);
    lines
        .iter()
        .map(|line| {
            let mut row = vec![String::new(); columns.len()];
            for segment in &line.segments {
                let index = nearest(&columns, segment.left);
                let cell = &mut row[index];
                if !cell.is_empty() {
                    cell.push(' ');
                }
                cell.push_str(segment.text.trim());
            }
            row
        })
        .collect()
}

fn nearest(columns: &[f32], x: f32) -> usize {
    let mut best = 0;
    let mut distance = f32::INFINITY;
    for (i, c) in columns.iter().enumerate() {
        if (c - x).abs() < distance {
            distance = (c - x).abs();
            best = i;
        }
    }
    best
}

/// Where the paragraph, heading or list item starting at `start` ends.
fn prose_end(lines: &[Line], start: usize, style: &Style, measure: f32) -> usize {
    let first = &lines[start];
    let level = style.level(first.size);
    let mut end = start + 1;

    while end < lines.len() {
        let previous = &lines[end - 1];
        let line = &lines[end];
        // A run of aligned gaps ahead is a table, and never part of this block.
        if table_end(lines, end).is_some() {
            break;
        }
        // Heading and body never share a block, and neither do two sizes.
        if style.level(line.size) != level || (line.size - first.size).abs() >= SIZE_EPSILON {
            break;
        }
        // A list marker always starts its own item.
        if marker_of(&line.text).is_some() {
            break;
        }
        let em = line.size.max(1.0);
        // A first-line indent starts a paragraph in books and reports.
        if line.left > first.left + INDENT_EMS * em {
            break;
        }
        // The line before ran short of the column's measure, so it finished a
        // paragraph rather than wrapping into this one. Only trusted for text
        // set flush left — in a centred or ragged block every line is "short".
        let flush_left = (line.left - first.left).abs() < 0.5 * em;
        if flush_left && previous.right < measure - SHORT_LINE_EMS * em {
            break;
        }
        end += 1;
    }
    end
}

fn prose(lines: &[Line], style: &Style, measure: f32) -> Block {
    let level = style.level(lines[0].size).or_else(|| bold_heading_level(lines, style, measure));
    let mut spans = join(lines);
    if let Some(level) = level {
        return Block::Heading { level, spans };
    }
    if let Some(marker) = marker_of(&lines[0].text) {
        strip_marker(&mut spans, marker.len());
        return Block::ListItem { marker, spans };
    }
    Block::Paragraph { spans }
}

/// A heading set in the body size, distinguished only by weight.
///
/// Plenty of papers set `4.5 Microsoft Research Sentence Completion Challenge`
/// in bold body type rather than at a larger size. It is a heading by every
/// other sign — one line, standing alone, well short of the measure — and
/// dropping it to a paragraph loses the document's whole section structure.
fn bold_heading_level(lines: &[Line], style: &Style, measure: f32) -> Option<u8> {
    let [line] = lines else { return None };
    if !line.mostly(|s| s.bold) || line.text.trim().is_empty() {
        return None;
    }
    // Body type or larger only: a bold caption set smaller is not a heading.
    if line.size < style.body() - SIZE_EPSILON {
        return None;
    }
    if line.right > measure - HEADING_SLACK_EMS * line.size.max(1.0) {
        return None;
    }
    // It ranks below every size-based level, since it is no larger than body.
    Some((style.levels() as u8 + 1).min(MAX_LEVEL))
}

/// Join wrapped lines into one run of spans, healing hyphenation where it is
/// safe to. A hyphen is only dropped when the join produces a single unhyphenated
/// word — so `informa-`/`tion` becomes `information` while `English-`/`to-German`
/// keeps the hyphen it was written with.
fn join(lines: &[Line]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let previous = out.last().map(|s| s.text.clone()).unwrap_or_default();
            match hyphen_join(&previous, &line.text) {
                // A soft hyphen: drop it and close the word up.
                Some(true) => {
                    if let Some(last) = out.last_mut() {
                        last.text.pop();
                    }
                }
                // A wrapped compound: keep the hyphen, but still close up.
                Some(false) => {}
                None => {
                    if let Some(last) = out.last_mut() {
                        last.text.push(' ');
                    }
                }
            }
        }
        for span in &line.spans {
            match out.last_mut() {
                // `link` is part of the identity of a span: merging a linked run
                // into the plain text before it silently drops the URI.
                Some(last)
                    if last.bold == span.bold
                        && last.italic == span.italic
                        && last.link == span.link =>
                {
                    last.text.push_str(&span.text)
                }
                _ => out.push(span.clone()),
            }
        }
    }
    out
}

/// `None` when the lines are separate words, `Some(drop_hyphen)` when they are
/// two halves of one — and whether the hyphen between them was the typesetter's
/// or the author's.
fn hyphen_join(previous: &str, next: &str) -> Option<bool> {
    let stem = previous.strip_suffix('-')?;
    let tail = stem.rsplit(char::is_whitespace).next().unwrap_or("");
    let head = next.trim_start().split_whitespace().next().unwrap_or("");
    if tail.is_empty() || head.is_empty() {
        return None;
    }
    if !tail.chars().last().is_some_and(|c| c.is_lowercase())
        || !head.chars().next().is_some_and(|c| c.is_lowercase())
    {
        return None;
    }
    // A hyphen already on either half means the word is a compound the author
    // wrote, so the break's hyphen belongs to it and stays.
    Some(!tail.contains('-') && !head.contains('-'))
}

/// The list marker a line opens with, normalised to markdown.
fn marker_of(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if "•◦▪‣●∙⁃*-–—".contains(first) && chars.next().is_some_and(char::is_whitespace) {
        return Some("-".to_string());
    }
    // `1.` / `1)` / `a.` / `a)`, but not `3.1` — a numbered section heading.
    let (label, rest) = trimmed.split_once([' ', '\t'])?;
    if rest.trim().is_empty() {
        return None;
    }
    let body = label.strip_suffix(['.', ')'])?;
    let body = body.strip_prefix('(').unwrap_or(body);
    if body.is_empty() || body.len() > 3 {
        return None;
    }
    if body.chars().all(|c| c.is_ascii_digit()) {
        return Some(format!("{body}."));
    }
    if body.len() == 1 && body.chars().all(|c| c.is_ascii_alphabetic()) {
        return Some("-".to_string());
    }
    None
}

/// Drop the source marker and the whitespace after it from the item's text.
fn strip_marker(spans: &mut Vec<Span>, _marker_len: usize) {
    let Some(first) = spans.first_mut() else { return };
    let trimmed = first.text.trim_start();
    let rest = match trimmed.split_once([' ', '\t']) {
        Some((_, rest)) => rest.trim_start().to_string(),
        None => String::new(),
    };
    first.text = rest;
    spans.retain(|s| !s.text.is_empty());
}

fn median_size(lines: &[Line]) -> f32 {
    let mut sizes: Vec<f32> = lines.iter().map(|l| l.size).filter(|s| *s > 0.0).collect();
    if sizes.is_empty() {
        return 10.0;
    }
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sizes[sizes.len() / 2]
}

/// Exposed so tests elsewhere can build a line without repeating the literal.
#[cfg(test)]
pub(crate) fn test_line(text: &str, baseline: f32, left: f32, right: f32, size: f32) -> Line {
    let segments = text
        .split("  ")
        .filter(|s| !s.trim().is_empty())
        .enumerate()
        .map(|(i, s)| Segment { text: s.trim().into(), left: left + i as f32 * 100.0, right: left + i as f32 * 100.0 + 50.0 })
        .collect();
    Line {
        spans: vec![Span { text: text.into(), bold: false, italic: false, link: None }],
        text: text.into(),
        segments,
        baseline,
        left,
        right,
        size,
        turn: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_lines() -> Vec<Line> {
        (0..12).map(|i| test_line(&format!("body text line {i}"), 700.0 - i as f32 * 12.0, 40.0, 550.0, 10.0)).collect()
    }

    #[test]
    fn heading_levels_rank_by_size_across_the_document() {
        let mut lines = body_lines();
        for i in 0..4 {
            lines.push(test_line("A Very Large Title Indeed", 400.0 - i as f32 * 20.0, 40.0, 550.0, 18.0));
            lines.push(test_line("A Section Heading Here Now", 380.0 - i as f32 * 20.0, 40.0, 550.0, 13.0));
        }
        let style = Style::of(&lines);
        assert_eq!(style.body(), 10.0);
        assert_eq!(style.level(18.0), Some(1));
        assert_eq!(style.level(13.0), Some(2));
        assert_eq!(style.level(10.0), None);
    }

    #[test]
    fn a_size_used_once_is_not_a_heading_level() {
        let mut lines = body_lines();
        lines.push(test_line("x", 300.0, 40.0, 60.0, 22.0));
        assert_eq!(Style::of(&lines).level(22.0), None);
    }

    #[test]
    fn wrapped_lines_join_into_one_paragraph() {
        let lines = vec![
            test_line("The first line runs the full measure", 700.0, 40.0, 550.0, 10.0),
            test_line("and the second continues it", 688.0, 40.0, 550.0, 10.0),
            test_line("short end.", 676.0, 40.0, 200.0, 10.0),
        ];
        let blocks = build(&lines, &Style::of(&lines));
        assert_eq!(blocks.len(), 1);
        let Block::Paragraph { spans } = &blocks[0] else { panic!("expected a paragraph") };
        assert_eq!(spans[0].text, "The first line runs the full measure and the second continues it short end.");
    }

    #[test]
    fn a_short_line_ends_the_paragraph_before_the_next_one() {
        let lines = vec![
            test_line("First paragraph ends here.", 700.0, 40.0, 200.0, 10.0),
            test_line("Second paragraph starts here and runs on", 688.0, 40.0, 550.0, 10.0),
        ];
        assert_eq!(build(&lines, &Style::of(&lines)).len(), 2);
    }

    #[test]
    fn hyphenation_heals_only_when_the_join_is_unambiguous() {
        let split_word = vec![
            test_line("carries the informa-", 700.0, 40.0, 550.0, 10.0),
            test_line("tion onward to the end of the line", 688.0, 40.0, 550.0, 10.0),
        ];
        let Block::Paragraph { spans } = &build(&split_word, &Style::of(&split_word))[0] else { panic!() };
        assert!(spans[0].text.starts_with("carries the information onward"), "{}", spans[0].text);

        let compound = vec![
            test_line("the English-", 700.0, 40.0, 550.0, 10.0),
            test_line("to-German task runs the full measure", 688.0, 40.0, 550.0, 10.0),
        ];
        let Block::Paragraph { spans } = &build(&compound, &Style::of(&compound))[0] else { panic!() };
        assert!(spans[0].text.starts_with("the English-to-German"), "{}", spans[0].text);
    }

    #[test]
    fn list_markers_are_recognised_and_stripped() {
        assert_eq!(marker_of("• first item"), Some("-".into()));
        assert_eq!(marker_of("1. first item"), Some("1.".into()));
        assert_eq!(marker_of("a) first item"), Some("-".into()));
        // A numbered section heading is not a list item.
        assert_eq!(marker_of("3.1 Encoder and Decoder"), None);
        assert_eq!(marker_of("plain prose"), None);
    }

    #[test]
    fn aligned_gaps_across_rows_become_a_table() {
        let lines = vec![
            test_line("Layer  Complexity  Ops", 700.0, 40.0, 550.0, 10.0),
            test_line("Recurrent  O(n)  O(1)", 688.0, 40.0, 550.0, 10.0),
            test_line("Convolutional  O(k)  O(1)", 676.0, 40.0, 550.0, 10.0),
        ];
        let blocks = build(&lines, &Style::of(&lines));
        let Block::Table { rows } = &blocks[0] else { panic!("expected a table, got {blocks:?}") };
        assert_eq!(rows[0], vec!["Layer", "Complexity", "Ops"]);
        assert_eq!(rows[2], vec!["Convolutional", "O(k)", "O(1)"]);
    }

    #[test]
    fn one_line_of_aligned_gaps_is_not_a_table() {
        let lines = vec![
            test_line("Caption here  17", 700.0, 40.0, 550.0, 10.0),
            test_line("ordinary prose continues below it", 688.0, 40.0, 550.0, 10.0),
        ];
        assert!(!matches!(build(&lines, &Style::of(&lines))[0], Block::Table { .. }));
    }
}
