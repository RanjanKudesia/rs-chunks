//! Glyphs → lines of text.
//!
//! A PDF says where each glyph is painted and nothing about words, lines or
//! reading order — those have to be recovered from geometry. Two steps:
//!
//! 1. **Band by baseline.** Glyphs sorted top-down join the current line while
//!    their baselines stay within half an em of it, which keeps superscripts and
//!    mixed-size runs on the line they belong to.
//! 2. **Space by gap.** Within a line, a horizontal gap wider than a fraction of
//!    an em is a word break. Producers that already emit space characters are not
//!    double-spaced.

use super::content::Glyph;

/// A gap this wide, relative to the em size around it, reads as a word break.
/// Below it lies ordinary letter spacing and the rounding producers introduce
/// when they position glyphs individually.
const WORD_GAP_EM: f32 = 0.2;

/// A gap this wide is not a word break but a *column* break: the run of
/// whitespace between two cells of a table row.
const CELL_GAP_EM: f32 = 1.3;

/// How far a baseline may sit from its line's, in ems, and still belong to it.
const BASELINE_TOLERANCE_EM: f32 = 0.5;

/// A run of characters sharing one style, so emphasis survives into markdown.
#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<std::rc::Rc<str>>,
}

/// A stretch of a line separated from its neighbours by a cell-sized gap.
#[derive(Debug, Clone)]
pub(crate) struct Segment {
    pub text: String,
    pub left: f32,
    pub right: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct Line {
    pub spans: Vec<Span>,
    pub text: String,
    /// The line's own cell candidates. A table row spans the full width, so it
    /// survives the region cut intact — its columns are recoverable only from
    /// the wide gaps inside it.
    pub segments: Vec<Segment>,
    pub baseline: f32,
    pub left: f32,
    pub right: f32,
    /// The size most of the line's characters are set in.
    pub size: f32,
    pub turn: u8,
}

impl Line {
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// True when nearly the whole line is set in one emphasis style — the only
    /// case where wrapping the line as a unit is faithful.
    pub fn mostly(&self, pick: fn(&Span) -> bool) -> bool {
        let (mut yes, mut total) = (0usize, 0usize);
        for span in &self.spans {
            let n = span.text.chars().filter(|c| !c.is_whitespace()).count();
            total += n;
            if pick(span) {
                yes += n;
            }
        }
        total > 0 && yes * 10 >= total * 9
    }
}

/// Group one page's glyphs into lines, in top-to-bottom order per reading frame.
pub(crate) fn build(glyphs: &[Glyph]) -> Vec<Line> {
    let mut turns: Vec<u8> = glyphs.iter().map(|g| g.turn).collect();
    turns.sort_unstable();
    turns.dedup();

    let mut out = Vec::new();
    for turn in turns {
        let mut frame: Vec<&Glyph> = glyphs.iter().filter(|g| g.turn == turn).collect();
        // Top-down, then left-to-right, so the sweep below only ever compares a
        // glyph against the band it is closest to.
        frame.sort_by(|a, b| {
            b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal).then(
                a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        out.extend(band(&frame, turn));
    }
    out
}

fn band(sorted: &[&Glyph], turn: u8) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut current: Vec<&Glyph> = Vec::new();
    let mut baseline = 0.0f32;
    let mut band_size = 0.0f32;

    for g in sorted {
        let tolerance = BASELINE_TOLERANCE_EM * g.size.max(band_size);
        if current.is_empty() || (baseline - g.y).abs() <= tolerance {
            if current.is_empty() {
                baseline = g.y;
            }
            band_size = band_size.max(g.size);
            current.push(g);
        } else {
            lines.push(assemble(&mut current, baseline, turn));
            baseline = g.y;
            band_size = g.size;
            current.push(g);
        }
    }
    if !current.is_empty() {
        lines.push(assemble(&mut current, baseline, turn));
    }
    lines
}

fn assemble(glyphs: &mut Vec<&Glyph>, baseline: f32, turn: u8) -> Line {
    glyphs.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

    let mut spans: Vec<Span> = Vec::new();
    let mut text = String::new();
    let mut segments: Vec<Segment> = Vec::new();
    let mut previous_end = f32::NEG_INFINITY;
    let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);

    for g in glyphs.iter() {
        let gap = g.x - previous_end;
        if previous_end.is_finite() && gap > CELL_GAP_EM * g.size {
            segments.push(Segment { text: String::new(), left: g.x, right: g.x });
        } else if segments.is_empty() {
            segments.push(Segment { text: String::new(), left: g.x, right: g.x });
        }
        let needs_space = previous_end.is_finite()
            && gap > WORD_GAP_EM * g.size
            && !text.ends_with(char::is_whitespace)
            && !g.text.starts_with(char::is_whitespace);
        if needs_space {
            push(&mut spans, &mut text, " ", g.bold, g.italic, g.link.clone());
            if let Some(seg) = segments.last_mut() {
                if !seg.text.is_empty() {
                    seg.text.push(' ');
                }
            }
        }
        push(&mut spans, &mut text, &g.text, g.bold, g.italic, g.link.clone());
        if let Some(seg) = segments.last_mut() {
            seg.text.push_str(&g.text);
            seg.right = g.x + g.width;
        }
        left = left.min(g.x);
        right = right.max(g.x + g.width);
        previous_end = g.x + g.width;
    }

    segments.retain(|s| !s.text.trim().is_empty());
    let size = dominant_size(glyphs);
    glyphs.clear();
    Line {
        spans,
        text,
        segments,
        baseline,
        left: if left.is_finite() { left } else { 0.0 },
        right: if right.is_finite() { right } else { 0.0 },
        size,
        turn,
    }
}

fn push(
    spans: &mut Vec<Span>,
    text: &mut String,
    piece: &str,
    bold: bool,
    italic: bool,
    link: Option<std::rc::Rc<str>>,
) {
    text.push_str(piece);
    match spans.last_mut() {
        Some(last) if last.bold == bold && last.italic == italic && last.link == link => {
            last.text.push_str(piece)
        }
        _ => spans.push(Span { text: piece.to_string(), bold, italic, link }),
    }
}

/// The size the most characters are set in — not the largest, so a single
/// oversized drop cap or inline formula cannot pass a line off as a heading.
fn dominant_size(glyphs: &[&Glyph]) -> f32 {
    let mut sizes: Vec<(f32, usize)> = Vec::new();
    for g in glyphs {
        let n = g.text.chars().filter(|c| !c.is_whitespace()).count();
        if n == 0 {
            continue;
        }
        match sizes.iter_mut().find(|(s, _)| (*s - g.size).abs() < 0.1) {
            Some((_, count)) => *count += n,
            None => sizes.push((g.size, n)),
        }
    }
    sizes
        .into_iter()
        .max_by_key(|(size, count)| (*count, (size * 100.0) as i64))
        .map(|(size, _)| size)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, x: f32, y: f32, size: f32) -> Glyph {
        Glyph {
            text: text.into(),
            x,
            y,
            width: size * 0.5 * text.chars().count() as f32,
            size,
            turn: 0,
            bold: false,
            italic: false,
            link: None,
        }
    }

    #[test]
    fn a_wide_gap_becomes_a_space_and_a_narrow_one_does_not() {
        // "on" then "e" butted up, then "two" after a 4pt gap at 10pt type.
        let glyphs = vec![glyph("on", 0.0, 100.0, 10.0), glyph("e", 10.0, 100.0, 10.0), glyph("two", 19.0, 100.0, 10.0)];
        let lines = build(&glyphs);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "one two");
    }

    #[test]
    fn a_superscript_stays_on_its_line() {
        let mut sup = glyph("2", 20.0, 103.0, 6.0);
        sup.width = 3.0;
        let glyphs = vec![glyph("x", 0.0, 100.0, 10.0), sup];
        assert_eq!(build(&glyphs).len(), 1);
    }

    #[test]
    fn a_new_baseline_starts_a_new_line() {
        let glyphs = vec![glyph("first", 0.0, 100.0, 10.0), glyph("second", 0.0, 88.0, 10.0)];
        let lines = build(&glyphs);
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].text.as_str(), lines[1].text.as_str()), ("first", "second"));
    }

    #[test]
    fn sideways_text_never_joins_a_horizontal_line() {
        let mut sideways = glyph("margin", 0.0, 100.0, 10.0);
        sideways.turn = 1;
        let glyphs = vec![glyph("body", 0.0, 100.0, 10.0), sideways];
        let lines = build(&glyphs);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.iter().map(|l| l.turn).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn a_cell_sized_gap_starts_a_new_segment() {
        // Three cells of a table row, each separated by well over an em.
        let glyphs = vec![
            glyph("Recurrent", 0.0, 100.0, 10.0),
            glyph("O(n)", 120.0, 100.0, 10.0),
            glyph("O(1)", 240.0, 100.0, 10.0),
        ];
        let line = &build(&glyphs)[0];
        assert_eq!(line.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["Recurrent", "O(n)", "O(1)"]);
    }

    #[test]
    fn ordinary_word_spacing_leaves_one_segment() {
        let glyphs = vec![glyph("one", 0.0, 100.0, 10.0), glyph("two", 19.0, 100.0, 10.0)];
        assert_eq!(build(&glyphs)[0].segments.len(), 1);
    }

    #[test]
    fn one_oversized_glyph_does_not_set_the_line_size() {
        let mut big = glyph("Q", 0.0, 100.0, 24.0);
        big.width = 12.0;
        let mut glyphs = vec![big];
        for i in 0..10 {
            glyphs.push(glyph("a", 12.0 + i as f32 * 5.0, 100.0, 10.0));
        }
        assert_eq!(build(&glyphs)[0].size, 10.0);
    }
}
