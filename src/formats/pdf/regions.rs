//! Reading order, by recursive projection cut (the classic XY-cut).
//!
//! The cut runs on *glyphs*, before lines exist. It has to: a two-column page
//! sets both columns on the same baseline grid, so grouping by baseline first
//! welds each left-hand line to the right-hand line beside it, and no later pass
//! can prise them apart. Cutting first and grouping inside each column is the
//! only order that reads a paper correctly.
//!
//! The page is cut alternately — horizontally wherever whitespace crosses the
//! full width, vertically wherever a gutter runs the full height — until neither
//! applies. Each leaf becomes a run of lines, and the leaves are already in
//! reading order.
//!
//! **A vertical cut is only believed for justified body columns.** A table's
//! cells are separated by the same kind of whitespace as a gutter, and reading
//! one down its columns garbles it. The two are told apart by whether the text
//! fills its measure: body columns are set to a common right edge, while cells
//! and a byline's stacked affiliations are ragged. A rejected cut leaves the
//! rows whole, which is what the table and byline passes downstream need.
//!
//! **Whitespace is measured exactly.** `empty_runs` merges the glyph extents and
//! reports the gaps between them, so a 9.76 pt gutter reads as 9.76 pt. It used
//! to mark 1 pt bins `floor(start) ..= ceil(end)` inclusively — one bin too far
//! on the right, and up to one more lost to flooring on the left — which
//! under-read every gap on every page by 1–2 pt and left the thresholds below
//! meaning nothing that could be checked against the page
//! ([#96](TECH_DEBT.md)). The exact form also drops a `1_000_000`-bin refusal
//! branch, because it no longer allocates anything proportional to page size.

use super::content::Glyph;
use super::lines::{self, Line};

/// A gutter must be at least this wide, and at least [`GUTTER_EMS`] of an em.
///
/// These were 9.0 / 1.1, which rejected `arxiv_1502.03167_batchnorm.pdf`'s real
/// two-column gutter and welded its columns into single lines (TECH_DEBT #94).
///
/// **These numbers now mean what they say.** They used to be tuned against a
/// profiler that under-reported every gap by 1–2 pt ([#96](TECH_DEBT.md)), so
/// `0.77` was never a claim about the page — `arxiv_1502.03167_batchnorm`'s
/// 9.76 pt gutter was only ever *seen* as 8.0 pt. `empty_runs` measures exactly
/// now, and the threshold sits where a ruler would put it: batchnorm's gutter
/// is 9.76 pt against a 9.96 pt em, **0.98 em**, and the measured value at
/// which its cut is lost is between 0.96 and 0.98. The agreement is the point —
/// it is what says the profiler and the page are talking about the same thing.
///
/// Measured window on the corpus, with the engine rather than a replica:
///
/// | `GUTTER_EMS` | batchnorm phantom tables | vgg Table 2 |
/// |---|---|---|
/// | ≤ 0.88 | 10 | **loses its `E` column** |
/// | 0.89 – 0.93 | **10** | intact |
/// | 0.94 – 0.96 | 12 | intact |
/// | ≥ 0.98 | 24 — the column cut is gone | intact |
///
/// So both ends are pinned by real damage: below 0.89 a genuine table splits
/// down the middle, and from 0.98 a genuine two-column page welds back into
/// rows the table pass then reads as a grid. **0.91** is the centre of
/// `[0.89, 0.93]`, where batchnorm is also at its best, with 0.02–0.03 em of
/// headroom either side.
///
/// `MIN_GUTTER` must stay below `0.91 × em` for body type or it re-binds and
/// `GUTTER_EMS` stops mattering — the trap [#94](TECH_DEBT.md) fell into, where
/// lowering `GUTTER_EMS` alone did nothing because `MIN_GUTTER` was 9.0.
///
/// **It is no longer inert, and the previous comment here saying so is wrong.**
/// That was measured against the binned profiler; with exact gaps, 0, 5, 7 and
/// 8 each move 2–4 fixtures. What moves is figure annotation — yolo's layer
/// dimensions, `pdfjs_comments`' callout labels — set small enough that
/// `0.91 × em` falls under the floor, and the direction is not monotonic,
/// because a *rejected* cut hands the region to the table pass instead. No
/// corpus fixture can say which reading of a figure label is right, so this
/// keeps the value it already had rather than being re-tuned on a preference.
/// Body text is untouched by it at any of those values.
const MIN_GUTTER: f32 = 6.0;
const GUTTER_EMS: f32 = 0.91;

/// Vertical whitespace beyond this fraction of an em separates blocks. Ordinary
/// leading leaves about a fifth of an em between one line's descenders and the
/// next line's ascenders.
///
/// **Raised from 0.50 with [#96](TECH_DEBT.md), and it had to be.** This is fed
/// by the same `empty_runs` as [`GUTTER_EMS`], so it was calibrated against the
/// same 1–2 pt under-measurement; once gaps read true, 0.50 split bands that
/// were never meant to split. Correcting only the gutter and leaving this alone
/// cost `pdfjs_comments` **half its headings** — `## 3.1 Traces` demoted to
/// bold, `3.3 Blacklisting` welding the word `Guard` out of a neighbouring
/// figure, and a paragraph of body prose pulled into a two-column table with a
/// figure legend. No value of `GUTTER_EMS` recovered it, which is what says the
/// fault was here.
///
/// Measured window, with the engine: `pdfjs_comments` holds its 30 headings and
/// 16 tables and batchnorm its 9 tables on `[0.70, 0.90]`; at 0.65 comments is
/// one heading short, at 0.55 it has 16 of 30, and from 1.00 bands stop
/// splitting that should (comments 28, batchnorm 8). **0.80** is the centre.
///
/// The correction is +0.20 em on a ~10 pt em — the 2 pt the old profiler lost.
/// That both constants moved by almost exactly the measurement error is the
/// strongest evidence the profiler is now reading the page correctly.
const BAND_GAP_EMS: f32 = 0.80;

/// A column needs this many lines before the split is believed.
const MIN_LINES_PER_COLUMN: usize = 2;

/// A line reaching within this fraction of its column's measure is "full".
const FULL_LINE_SLACK: f32 = 0.04;

/// Columns of body text are justified, so most lines run the full measure. Below
/// this share, the block is a grid and its rows must stay intact.
const JUSTIFIED_SHARE: f32 = 0.5;

/// How deep the alternating cuts may go. Real pages terminate in three or four.
const MAX_DEPTH: usize = 10;

/// One run of lines that reads straight through.
pub(crate) type Region = Vec<Line>;

/// Split one page's glyphs into regions of lines, in reading order.
pub(crate) fn split(glyphs: &[Glyph]) -> Vec<Region> {
    let mut turns: Vec<u8> = glyphs.iter().map(|g| g.turn).collect();
    turns.sort_unstable();
    turns.dedup();

    let mut out = Vec::new();
    // Upright text first; sideways text follows in its own frame rather than
    // being interleaved by height.
    for turn in turns {
        let frame: Vec<&Glyph> = glyphs.iter().filter(|g| g.turn == turn).collect();
        cut(&frame, 0, true, &mut out);
    }
    out
}

fn cut(glyphs: &[&Glyph], depth: usize, horizontal_first: bool, out: &mut Vec<Region>) {
    if glyphs.is_empty() {
        return;
    }
    if depth >= MAX_DEPTH {
        emit(glyphs, out);
        return;
    }
    let order: [bool; 2] = if horizontal_first { [true, false] } else { [false, true] };
    for horizontal in order {
        let parts = if horizontal { bands(glyphs) } else { columns(glyphs) };
        if parts.len() > 1 {
            for part in parts {
                cut(&part, depth + 1, !horizontal, out);
            }
            return;
        }
    }
    emit(glyphs, out);
}

fn emit(glyphs: &[&Glyph], out: &mut Vec<Region>) {
    let owned: Vec<Glyph> = glyphs.iter().map(|g| (*g).clone()).collect();
    let region = lines::build(&owned);
    if !region.is_empty() {
        out.push(region);
    }
}

/// Split at whitespace crossing the full width.
fn bands<'a>(glyphs: &[&'a Glyph]) -> Vec<Vec<&'a Glyph>> {
    let em = median_size(glyphs);
    let extents: Vec<(f32, f32)> = glyphs.iter().map(|g| (g.y - 0.22 * g.size, g.y + 0.78 * g.size)).collect();
    let gaps = empty_runs(&extents, (BAND_GAP_EMS * em).max(1.0));
    if gaps.is_empty() {
        return vec![glyphs.to_vec()];
    }
    // Counting the gaps *above* a glyph numbers the bands top-down, which is
    // the order they are read in.
    let mut parts: Vec<Vec<&Glyph>> = vec![Vec::new(); gaps.len() + 1];
    for g in glyphs {
        let index = gaps.iter().filter(|edge| g.y < **edge).count();
        parts[index].push(g);
    }
    parts
}

/// Split at a gutter running the full height — but only for justified columns.
fn columns<'a>(glyphs: &[&'a Glyph]) -> Vec<Vec<&'a Glyph>> {
    let single = || vec![glyphs.to_vec()];
    let em = median_size(glyphs);
    let extents: Vec<(f32, f32)> = glyphs.iter().map(|g| (g.x, g.x + g.width)).collect();
    let edges = empty_runs(&extents, MIN_GUTTER.max(GUTTER_EMS * em));
    if edges.is_empty() {
        return single();
    }

    let mut parts: Vec<Vec<&Glyph>> = vec![Vec::new(); edges.len() + 1];
    for g in glyphs {
        let index = edges.iter().filter(|edge| g.x >= **edge).count();
        parts[index].push(g);
    }
    for part in &parts {
        if !reads_as_a_column(part) {
            return single();
        }
    }
    parts
}

/// Whether a candidate column looks like justified body text rather than a
/// stack of table cells.
fn reads_as_a_column(glyphs: &[&Glyph]) -> bool {
    let owned: Vec<Glyph> = glyphs.iter().map(|g| (*g).clone()).collect();
    let lines = lines::build(&owned);
    if lines.len() < MIN_LINES_PER_COLUMN {
        return false;
    }
    let measure = lines.iter().map(|l| l.right).fold(f32::NEG_INFINITY, f32::max);
    let left = lines.iter().map(|l| l.left).fold(f32::INFINITY, f32::min);
    let width = measure - left;
    if width <= 0.0 {
        return false;
    }
    let full = lines.iter().filter(|l| l.right >= measure - width * FULL_LINE_SLACK).count();
    full as f32 >= lines.len() as f32 * JUSTIFIED_SHARE
}

/// The midpoints of every interior run of empty bins at least `min_width` wide.
fn empty_runs(extents: &[(f32, f32)], min_width: f32) -> Vec<f32> {
    let mut spans: Vec<(f32, f32)> =
        extents.iter().copied().filter(|(start, end)| start.is_finite() && end >= start).collect();
    if spans.is_empty() {
        return Vec::new();
    }
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = Vec::new();
    // Sweeping from the first glyph's end to the last glyph's start reports
    // interior gaps only — the outer margins are never between two glyphs, and
    // the binned version needed an explicit `start > 0` test to say so.
    let mut reach = spans[0].1;
    for &(start, end) in &spans[1..] {
        if start - reach >= min_width {
            out.push((reach + start) / 2.0);
        }
        reach = reach.max(end);
    }
    out
}

fn median_size(glyphs: &[&Glyph]) -> f32 {
    let mut sizes: Vec<f32> = glyphs.iter().map(|g| g.size).filter(|s| *s > 0.0).collect();
    if sizes.is_empty() {
        return 10.0;
    }
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sizes[sizes.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One glyph per character, laid out on a 5 pt grid at 10 pt type.
    fn run(text: &str, x: f32, y: f32) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| Glyph {
                text: c.to_string(),
                x: x + i as f32 * 5.0,
                y,
                width: 5.0,
                size: 10.0,
                turn: 0,
                bold: false,
                italic: false,
                link: None,
            })
            .collect()
    }

    fn texts(regions: Vec<Region>) -> Vec<String> {
        regions.into_iter().flatten().map(|l| l.text).collect()
    }

    /// Two justified columns, 40 characters wide, sharing a baseline grid.
    fn two_columns() -> Vec<Glyph> {
        let mut glyphs = Vec::new();
        for i in 0..6 {
            let y = 600.0 - i as f32 * 12.0;
            glyphs.extend(run(&format!("left{i} filling out the whole measure"), 40.0, y));
            glyphs.extend(run(&format!("right{i} filling out the whole measur"), 320.0, y));
        }
        glyphs
    }

    #[test]
    fn a_two_column_page_is_read_down_then_across() {
        let out = texts(split(&two_columns()));
        assert_eq!(out.len(), 12);
        assert!(out[..6].iter().all(|t| t.starts_with("left")), "{out:?}");
        assert!(out[6..].iter().all(|t| t.starts_with("right")), "{out:?}");
    }

    #[test]
    fn a_single_column_page_keeps_its_order() {
        let mut glyphs = Vec::new();
        for i in 0..6 {
            glyphs.extend(run(&format!("line{i} of a single column of prose"), 40.0, 600.0 - i as f32 * 12.0));
        }
        let out = texts(split(&glyphs));
        assert_eq!(out.len(), 6);
        assert!(out[0].starts_with("line0") && out[5].starts_with("line5"), "{out:?}");
    }

    /// A table's cells are separated by gutter-width whitespace too. Reading it
    /// down the columns would pair every value with the wrong row, so the cut
    /// must be refused and the rows left whole.
    #[test]
    fn a_ragged_grid_is_not_cut_into_columns() {
        let mut glyphs = Vec::new();
        for (i, (a, b)) in [("Recurrent", "O(n)"), ("Convolutional", "O(k)"), ("Self-Attention", "O(1)")]
            .iter()
            .enumerate()
        {
            let y = 600.0 - i as f32 * 12.0;
            glyphs.extend(run(a, 40.0, y));
            glyphs.extend(run(b, 300.0, y));
        }
        let out = texts(split(&glyphs));
        assert_eq!(out.len(), 3);
        assert!(out[0].starts_with("Recurrent") && out[0].ends_with("O(n)"), "{out:?}");
    }

    #[test]
    fn a_full_width_line_ends_the_columns_above_it() {
        let mut glyphs = two_columns();
        glyphs.extend(run("A FULL WIDTH HEADING ACROSS THE ENTIRE PAGE WIDTH", 40.0, 700.0));
        let out = texts(split(&glyphs));
        assert!(out[0].starts_with("A FULL WIDTH"), "{out:?}");
    }

    #[test]
    fn sideways_text_is_kept_out_of_the_upright_flow() {
        let mut glyphs = Vec::new();
        for i in 0..4 {
            glyphs.extend(run(&format!("body{i} of an ordinary upright page"), 40.0, 600.0 - i as f32 * 12.0));
        }
        let mut stamp = run("arXiv:1706.03762", 0.0, 300.0);
        for g in &mut stamp {
            g.turn = 1;
        }
        glyphs.extend(stamp);
        let out = texts(split(&glyphs));
        assert_eq!(out.last().unwrap(), "arXiv:1706.03762");
    }
}
