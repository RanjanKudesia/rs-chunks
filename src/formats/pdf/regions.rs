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

use super::content::Glyph;
use super::lines::{self, Line};

/// Bin width for the whitespace profiles, in points.
const BIN: f32 = 1.0;

/// A gutter must be at least this wide, and at least [`GUTTER_EMS`] of an em.
///
/// These were 9.0 / 1.1, which rejected `arxiv_1502.03167_batchnorm.pdf`'s real
/// two-column gutter and welded its columns into single lines (TECH_DEBT #94).
///
/// Two things about the measurement are worth writing down, because both are
/// counter-intuitive:
///
/// 1. **`empty_runs` under-reports every gap.** It marks bins
///    `floor(start) ..= ceil(end)` inclusively, expanding each glyph outward by
///    up to two bins, so batchnorm's 9.76 pt gutter is only ever *seen* as
///    8.0 pt (0.80 em). The constants therefore have to be tuned to what the
///    profiler measures, not to what a ruler would say. Fixing the binning
///    would let these numbers mean what they claim, but it perturbs every gap
///    on every page — a much larger change than this one ([#96](TECH_DEBT.md)).
/// 2. **Lowering `GUTTER_EMS` alone does nothing**, because `MIN_GUTTER` then
///    becomes the binding constraint: `max(9.0, 0.9 × 9.96)` is still 9.0 > 8.0.
///    Both had to move, which the tracker entry did not say.
///
/// Measured window on the corpus: a cut fires for batchnorm at `GUTTER_EMS
/// ≤ 0.803`, and at `≤ 0.72` `pdfjs_issue1905` starts *losing* a correct cut
/// while at `≤ 0.62` `arxiv_1409.1556_vgg`'s Table 2 splits down the middle —
/// the table damage a looser threshold is supposed to risk. 0.77 is the centre
/// of `[0.73, 0.803]`, leaving 0.03 em of headroom on each side.
///
/// `MIN_GUTTER` must sit below `0.77 × em` or it re-binds; 6.0 is inert across
/// the whole corpus (identical output for 0, 5, 6, 7 and 8) and is kept only as
/// a floor for very small type, which this corpus cannot calibrate.
const MIN_GUTTER: f32 = 6.0;
const GUTTER_EMS: f32 = 0.77;

/// Vertical whitespace beyond this fraction of an em separates blocks. Ordinary
/// leading leaves about a fifth of an em between one line's descenders and the
/// next line's ascenders.
const BAND_GAP_EMS: f32 = 0.5;

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
    let low = extents.iter().map(|e| e.0).fold(f32::INFINITY, f32::min);
    let high = extents.iter().map(|e| e.1).fold(f32::NEG_INFINITY, f32::max);
    let span = high - low;
    if !span.is_finite() || span <= min_width {
        return Vec::new();
    }
    let count = (span / BIN).ceil() as usize + 1;
    // A page big enough to blow this budget is malformed; refuse to profile it
    // rather than allocate from its numbers.
    if count > 1_000_000 {
        return Vec::new();
    }
    let mut covered = vec![false; count];
    for (start, end) in extents {
        let from = ((start - low) / BIN).floor().max(0.0) as usize;
        let to = (((end - low) / BIN).ceil() as usize).min(count - 1);
        for slot in covered.iter_mut().take(to + 1).skip(from) {
            *slot = true;
        }
    }

    let mut out = Vec::new();
    let mut run: Option<usize> = None;
    for (i, filled) in covered.iter().enumerate() {
        match (filled, run) {
            (false, None) => run = Some(i),
            (true, Some(start)) => {
                // `start == 0` is the outer margin, not an interior gap; the
                // loop never reaches the far margin because the last bin is set.
                if start > 0 && (i - start) as f32 * BIN >= min_width {
                    out.push(low + (start + i) as f32 / 2.0 * BIN);
                }
                run = None;
            }
            _ => {}
        }
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
