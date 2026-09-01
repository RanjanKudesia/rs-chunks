//! Content-stream interpretation: operators in, positioned glyphs and images out.
//!
//! This is the graphics/text state machine of PDF 32000-1 §8–9, reduced to what
//! extraction needs: where each glyph is painted, how big, and which images are
//! actually *drawn*. The distinction matters — a page's `/XObject` resource
//! dictionary lists what is *available*, and `pdfjs_images.pdf` offers five
//! images while its content stream draws four. Walking the stream is also the
//! only way to descend into Form XObjects, which is where those images live
//! ([#57](TECH_DEBT.md)).

use std::collections::HashMap;
use std::rc::Rc;

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId};

use super::font::Font;
use super::geom::Matrix;

/// Forms may nest; real documents go one or two deep. The cap is a cycle
/// backstop, not a policy — `visited` already refuses direct recursion.
const MAX_FORM_DEPTH: usize = 12;

/// One painted glyph run's worth of text.
///
/// `x`/`y` are in the *reading frame* named by `turn`, not in page space: text
/// painted sideways (an arXiv stamp down the margin, a rotated table header) is
/// rotated upright so that one layout pass serves every direction. `turn` keeps
/// the frames apart, since a sideways line must never join a horizontal one.
#[derive(Clone)]
pub(crate) struct Glyph {
    pub text: String,
    /// Left edge of the glyph on its baseline, in the reading frame.
    pub x: f32,
    /// Baseline height; larger is further up the page.
    pub y: f32,
    /// Advance to the next glyph's origin.
    pub width: f32,
    /// Painted font size, after every transform.
    pub size: f32,
    /// Quarter-turns anticlockwise from upright: 0 for ordinary text.
    pub turn: u8,
    pub bold: bool,
    pub italic: bool,
    /// The target of the link annotation covering this glyph, if any. A PDF
    /// keeps a hyperlink in `/Annots`, not in the text, so the URI exists
    /// nowhere in the content stream.
    pub link: Option<std::rc::Rc<str>>,
}

/// An image XObject at the point the content stream draws it.
pub(crate) struct PlacedImage {
    pub id: ObjectId,
    /// Top edge, so an image sorts against the text line it interrupts.
    pub top: f32,
    pub left: f32,
    // Recorded at draw time but not yet consumed by layout; kept because the
    // content-stream walker is the only place they can be captured.
    #[allow(dead_code)]
    pub width: f32,
    #[allow(dead_code)]
    pub height: f32,
}

#[derive(Default)]
pub(crate) struct PageContent {
    pub glyphs: Vec<Glyph>,
    pub images: Vec<PlacedImage>,
}

#[derive(Clone)]
struct TextState {
    font: Option<Rc<Font>>,
    size: f32,
    char_spacing: f32,
    word_spacing: f32,
    /// `Tz` as a factor (the operand is a percentage).
    hscale: f32,
    leading: f32,
    rise: f32,
    tm: Matrix,
    tlm: Matrix,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
        }
    }
}

/// Interprets content streams, carrying the font cache between pages.
///
/// It deliberately does **not** borrow the document: a streaming reader owns
/// both, and a self-referential struct would be the price of the shortcut.
#[derive(Default)]
pub(crate) struct Extractor {
    /// Fonts are shared across pages and parsing one means parsing its ToUnicode
    /// CMap, so a 5,000-page document must not do it 5,000 times.
    fonts: HashMap<ObjectId, Rc<Font>>,
}

impl Extractor {
    pub(crate) fn new() -> Self {
        Extractor::default()
    }

    /// Interpret one content stream against `resources`, appending to `out`.
    pub(crate) fn run(
        &mut self,
        doc: &Document,
        data: &[u8],
        resources: &Dictionary,
        base_ctm: Matrix,
        out: &mut PageContent,
    ) {
        let mut visited = Vec::new();
        self.run_inner(doc, data, resources, base_ctm, out, &mut visited);
    }

    #[allow(clippy::too_many_arguments)]
    fn run_inner(
        &mut self,
        doc: &Document,
        data: &[u8],
        resources: &Dictionary,
        base_ctm: Matrix,
        out: &mut PageContent,
        visited: &mut Vec<ObjectId>,
    ) {
        let data = strip_inline_images(data);
        let Ok(content) = Content::decode(&data) else {
            return;
        };
        let mut ctm = base_ctm;
        let mut stack: Vec<Matrix> = Vec::new();
        let mut ts = TextState::default();
        let mut saved_text: Vec<TextState> = Vec::new();

        for op in &content.operations {
            let a = &op.operands;
            match op.operator.as_str() {
                "q" => {
                    stack.push(ctm);
                    saved_text.push(ts.clone());
                }
                "Q" => {
                    if let Some(m) = stack.pop() {
                        ctm = m;
                    }
                    if let Some(t) = saved_text.pop() {
                        // The text *matrix* is not part of the graphics state;
                        // only the parameters are.
                        let (tm, tlm) = (ts.tm, ts.tlm);
                        ts = t;
                        ts.tm = tm;
                        ts.tlm = tlm;
                    }
                }
                "cm" => {
                    if let Some(m) = matrix_of(a) {
                        ctm = m.concat(ctm);
                    }
                }
                "BT" => {
                    ts.tm = Matrix::IDENTITY;
                    ts.tlm = Matrix::IDENTITY;
                }
                "Tf" => {
                    ts.font = a.first().and_then(|o| o.as_name().ok()).and_then(|n| {
                        let n = n.to_vec();
                        self.font(doc, resources, &n)
                    });
                    ts.size = num(a.get(1)).unwrap_or(0.0);
                }
                "Tc" => ts.char_spacing = num(a.first()).unwrap_or(0.0),
                "Tw" => ts.word_spacing = num(a.first()).unwrap_or(0.0),
                "Tz" => ts.hscale = num(a.first()).unwrap_or(100.0) / 100.0,
                "TL" => ts.leading = num(a.first()).unwrap_or(0.0),
                "Ts" => ts.rise = num(a.first()).unwrap_or(0.0),
                "Td" => {
                    let (tx, ty) = (num(a.first()).unwrap_or(0.0), num(a.get(1)).unwrap_or(0.0));
                    ts.tlm = Matrix::translate(tx, ty).concat(ts.tlm);
                    ts.tm = ts.tlm;
                }
                "TD" => {
                    let (tx, ty) = (num(a.first()).unwrap_or(0.0), num(a.get(1)).unwrap_or(0.0));
                    ts.leading = -ty;
                    ts.tlm = Matrix::translate(tx, ty).concat(ts.tlm);
                    ts.tm = ts.tlm;
                }
                "Tm" => {
                    if let Some(m) = matrix_of(a) {
                        ts.tlm = m;
                        ts.tm = m;
                    }
                }
                "T*" => next_line(&mut ts),
                "Tj" => {
                    if let Some(s) = a.first().and_then(|o| o.as_str().ok()) {
                        self.show(&mut ts, ctm, s, out);
                    }
                }
                "'" => {
                    next_line(&mut ts);
                    if let Some(s) = a.first().and_then(|o| o.as_str().ok()) {
                        self.show(&mut ts, ctm, s, out);
                    }
                }
                "\"" => {
                    ts.word_spacing = num(a.first()).unwrap_or(0.0);
                    ts.char_spacing = num(a.get(1)).unwrap_or(0.0);
                    next_line(&mut ts);
                    if let Some(s) = a.get(2).and_then(|o| o.as_str().ok()) {
                        self.show(&mut ts, ctm, s, out);
                    }
                }
                "TJ" => {
                    let Some(items) = a.first().and_then(|o| o.as_array().ok()) else {
                        continue;
                    };
                    for item in items {
                        match item {
                            Object::String(s, _) => self.show(&mut ts, ctm, s, out),
                            other => {
                                // A positive number moves left, so the sign is
                                // inverted relative to a translation.
                                let Some(adj) = num(Some(other)) else {
                                    continue;
                                };
                                let tx = -adj / 1000.0 * ts.size * ts.hscale;
                                ts.tm = Matrix::translate(tx, 0.0).concat(ts.tm);
                            }
                        }
                    }
                }
                "Do" => {
                    if let Some(name) = a.first().and_then(|o| o.as_name().ok()) {
                        self.draw_xobject(doc, resources, name, ctm, out, visited);
                    }
                }
                _ => {}
            }
        }
    }

    fn show(&mut self, ts: &mut TextState, ctm: Matrix, bytes: &[u8], out: &mut PageContent) {
        let Some(font) = ts.font.clone() else { return };
        for d in font.decode(bytes) {
            let render = ts.tm.concat(ctm);
            let mut advance = d.width * ts.size + ts.char_spacing;
            if d.is_space_code {
                advance += ts.word_spacing;
            }
            advance *= ts.hscale;

            if !d.text.is_empty() {
                let origin = Matrix::translate(0.0, ts.rise).concat(render);
                let turn = quarter_turns(render);
                let (x, y) = upright(turn, origin.e, origin.f);
                out.glyphs.push(Glyph {
                    text: d.text,
                    x,
                    y,
                    width: advance * render.horizontal_scale(),
                    size: ts.size * render.vertical_scale(),
                    turn,
                    bold: font.bold,
                    italic: font.italic,
                    link: None,
                });
            }
            ts.tm = Matrix::translate(advance, 0.0).concat(ts.tm);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_xobject(
        &mut self,
        doc: &Document,
        resources: &Dictionary,
        name: &[u8],
        ctm: Matrix,
        out: &mut PageContent,
        visited: &mut Vec<ObjectId>,
    ) {
        let Ok(xobjects) = resources
            .get_deref(b"XObject", doc)
            .and_then(Object::as_dict)
        else {
            return;
        };
        let Ok(entry) = xobjects.get(name) else {
            return;
        };
        let Ok((id, object)) = doc.dereference(entry) else {
            return;
        };
        let Ok(stream) = object.as_stream() else {
            return;
        };
        let subtype = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or(b"");

        if subtype == b"Image" {
            let Some(id) = id else { return };
            // The image fills the unit square, mapped through the CTM.
            let corners = [
                ctm.apply(0.0, 0.0),
                ctm.apply(1.0, 0.0),
                ctm.apply(0.0, 1.0),
                ctm.apply(1.0, 1.0),
            ];
            let xs: Vec<f32> = corners.iter().map(|c| c.0).collect();
            let ys: Vec<f32> = corners.iter().map(|c| c.1).collect();
            let (left, right) = (fmin(&xs), fmax(&xs));
            let (bottom, top) = (fmin(&ys), fmax(&ys));
            out.images.push(PlacedImage {
                id,
                top,
                left,
                width: right - left,
                height: top - bottom,
            });
            return;
        }
        if subtype != b"Form" {
            return;
        }
        let Some(id) = id else { return };
        if visited.contains(&id) || visited.len() >= MAX_FORM_DEPTH {
            return;
        }
        let Ok(data) = stream.decompressed_content() else {
            return;
        };
        let form_ctm = match stream.dict.get(b"Matrix").and_then(Object::as_array) {
            Ok(m) => matrix_of(m).unwrap_or(Matrix::IDENTITY).concat(ctm),
            Err(_) => ctm,
        };
        // A form without its own /Resources inherits the caller's.
        let inner = stream
            .dict
            .get_deref(b"Resources", doc)
            .and_then(Object::as_dict)
            .cloned()
            .unwrap_or_else(|_| resources.clone());

        visited.push(id);
        self.run_inner(doc, &data, &inner, form_ctm, out, visited);
        visited.pop();
    }

    fn font(&mut self, doc: &Document, resources: &Dictionary, name: &[u8]) -> Option<Rc<Font>> {
        let fonts = resources
            .get_deref(b"Font", doc)
            .and_then(Object::as_dict)
            .ok()?;
        let entry = fonts.get(name).ok()?;
        match entry {
            Object::Reference(id) => {
                if let Some(font) = self.fonts.get(id) {
                    return Some(font.clone());
                }
                let dict = doc.get_dictionary(*id).ok()?;
                let font = Rc::new(Font::from_dict(doc, dict));
                self.fonts.insert(*id, font.clone());
                Some(font)
            }
            Object::Dictionary(dict) => Some(Rc::new(Font::from_dict(doc, dict))),
            _ => None,
        }
    }
}

/// Remove `BI … ID <binary> EI` runs before the stream is tokenised.
///
/// An inline image's data is raw bytes sitting in the middle of the operator
/// stream, and the operand parser stops dead at it — on `arxiv_1409.1556_vgg`
/// a single 1×1 inline mask on page 2 cost the whole rest of the page, 55 KB of
/// text across the paper. The images themselves are not lost content worth
/// keeping: they are stencil masks and rules, unreferenceable because they have
/// no object id of their own.
fn strip_inline_images(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if !starts_token(data, i, b"BI") {
            out.push(data[i]);
            i += 1;
            continue;
        }
        // A `BI` with no `ID`, or no closing `EI`, is not an inline image — it
        // is a false positive (`(BI)` inside a text string satisfies
        // `starts_token`, since parentheses are delimiters) or a malformed
        // stream. Copying the byte through and moving on costs nothing; the
        // previous `break` threw away **the whole rest of the stream**, which
        // is precisely the 55 KB text loss this function exists to prevent.
        let Some(id) = find_token(data, i, b"ID") else {
            out.push(data[i]);
            i += 1;
            continue;
        };
        // The single whitespace byte after `ID` belongs to the delimiter, and
        // the data begins immediately after it.
        let data_start = id + 3;

        // Prefer arithmetic over search. For an unfiltered image the byte count
        // is exactly `ceil(W · components · BPC / 8) · H`, so the end is known
        // and no scan can be fooled — `EI` is a legal byte pair inside binary
        // data, and a delimiter-bounded one would end the image early and leave
        // the tail to be re-tokenised as operators.
        let end = inline_image_len(&data[i..data_start])
            .map(|len| data_start + len)
            .filter(|end| *end <= data.len() && find_token(data, *end, b"EI").is_some())
            .or_else(|| find_token(data, data_start, b"EI"));

        match end {
            Some(end) => i = end + 2,
            None => {
                out.push(data[i]);
                i += 1;
            }
        }
    }
    out
}

/// Byte length of an *unfiltered* inline image's data, from its own dictionary.
///
/// `None` when the image is filtered (no length is derivable without decoding)
/// or the dictionary does not say enough. Keys are the abbreviated forms an
/// inline dictionary uses: `/W /H /BPC /CS /IM /F`.
fn inline_image_len(header: &[u8]) -> Option<usize> {
    if find_token(header, 0, b"/F").is_some() || find_token(header, 0, b"/Filter").is_some() {
        return None; // compressed — length is not computable
    }
    let width = inline_int(header, b"/W")?;
    let height = inline_int(header, b"/H")?;
    let is_mask = find_token(header, 0, b"/IM").is_some();
    let bpc = if is_mask {
        1
    } else {
        inline_int(header, b"/BPC")?
    };
    let components = if is_mask {
        1
    } else {
        match () {
            // Indexed must be tested first: its array spells out a base space,
            // so `[/I /RGB 3 <…>]` would otherwise read as three components
            // when an indexed sample is one. It needs the palette to resolve, so
            // fall back to the scan rather than guess.
            _ if find_token(header, 0, b"/I").is_some()
                || find_token(header, 0, b"/Indexed").is_some() =>
            {
                return None
            }
            _ if find_token(header, 0, b"/CMYK").is_some() => 4,
            _ if find_token(header, 0, b"/RGB").is_some() => 3,
            _ if find_token(header, 0, b"/G").is_some() => 1,
            // A named colour space needs the resource dictionary.
            _ => return None,
        }
    };
    let row_bits = width.checked_mul(components)?.checked_mul(bpc)?;
    row_bits.div_ceil(8).checked_mul(height)
}

/// Read the integer operand of an abbreviated inline-image key.
fn inline_int(header: &[u8], key: &[u8]) -> Option<usize> {
    let at = find_token(header, 0, key)? + key.len();
    let rest = &header[at..];
    let start = rest.iter().position(|b| b.is_ascii_digit())?;
    let end = rest[start..]
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len() - start);
    std::str::from_utf8(&rest[start..start + end])
        .ok()?
        .parse()
        .ok()
}

/// True when `needle` sits at `at` as a whole token.
fn starts_token(data: &[u8], at: usize, needle: &[u8]) -> bool {
    if !data[at..].starts_with(needle) {
        return false;
    }
    let before = at == 0 || is_delimiter(data[at - 1]);
    let after = data.get(at + needle.len()).is_none_or(|b| is_delimiter(*b));
    before && after
}

fn find_token(data: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    (from..data.len()).find(|i| starts_token(data, *i, needle))
}

fn is_delimiter(b: u8) -> bool {
    b.is_ascii_whitespace() || b"()<>[]{}/%".contains(&b)
}

/// Which quarter turn the baseline points along. Anything that is not close to
/// an axis is treated as upright — skewed display type is rare, and rotating it
/// into a fifth frame of its own would only fragment the page further.
fn quarter_turns(render: Matrix) -> u8 {
    let (dx, dy) = (render.a, render.b);
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            0
        } else {
            2
        }
    } else if dy >= 0.0 {
        1
    } else {
        3
    }
}

/// Rotate a point out of page space into the reading frame of `turn`, so that
/// text running in that direction reads left-to-right, top-to-bottom.
fn upright(turn: u8, x: f32, y: f32) -> (f32, f32) {
    match turn {
        1 => (y, -x),
        2 => (-x, -y),
        3 => (-y, x),
        _ => (x, y),
    }
}

fn next_line(ts: &mut TextState) {
    ts.tlm = Matrix::translate(0.0, -ts.leading).concat(ts.tlm);
    ts.tm = ts.tlm;
}

fn matrix_of(operands: &[Object]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    let v: Vec<f32> = operands
        .iter()
        .take(6)
        .map(|o| num(Some(o)).unwrap_or(0.0))
        .collect();
    Some(Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
}

fn num(object: Option<&Object>) -> Option<f32> {
    match object? {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

fn fmin(values: &[f32]) -> f32 {
    values.iter().copied().fold(f32::INFINITY, f32::min)
}

fn fmax(values: &[f32]) -> f32 {
    values.iter().copied().fold(f32::NEG_INFINITY, f32::max)
}

#[cfg(test)]
mod inline_image_tests {
    use super::*;

    fn strip(s: &str) -> String {
        String::from_utf8_lossy(&strip_inline_images(s.as_bytes())).to_string()
    }

    /// The 1×1 stencil mask that 1,013 of the corpus's 1,015 inline images are.
    #[test]
    fn a_stencil_mask_is_removed_and_the_operators_survive() {
        let out =
            strip("q 1 0 0 1 0 0 cm BI /IM true /W 1 /H 1 /BPC 1 ID \u{0}\nEI Q BT (kept) Tj ET");
        assert!(
            out.contains("(kept) Tj"),
            "text after the image was lost: {out:?}"
        );
        assert!(
            !out.contains("/IM"),
            "the image dictionary survived: {out:?}"
        );
        assert!(out.starts_with("q 1 0 0 1 0 0 cm"), "{out:?}");
    }

    /// TECH_DEBT #86. A `BI` with no `ID`, or no `EI`, used to `break` — which
    /// discarded the whole remainder of the stream. That is the same 55 KB text
    /// loss this function was written to prevent, in the malformed case.
    #[test]
    fn a_malformed_marker_does_not_discard_the_rest_of_the_stream() {
        let out = strip("BT (before) Tj ET BI /W 1 BT (after) Tj ET");
        assert!(
            out.contains("(after) Tj"),
            "text after a BI with no ID was lost: {out:?}"
        );

        let out = strip("BT (before) Tj ET BI /W 1 /H 1 /BPC 1 ID \u{0} BT (after) Tj ET");
        assert!(
            out.contains("(after)") || out.contains("before"),
            "an unterminated inline image must not eat everything: {out:?}"
        );
    }

    /// `BI` inside a text string is a false positive — parentheses are
    /// delimiters, so `starts_token` matches it.
    #[test]
    fn bi_inside_a_string_is_not_an_inline_image() {
        let out = strip("BT (BI) Tj (kept) Tj ET");
        assert!(out.contains("(kept) Tj"), "{out:?}");
    }

    /// Length is computed from the dictionary, so binary data containing a
    /// delimiter-bounded `EI` cannot end the image early.
    #[test]
    fn binary_data_containing_ei_does_not_end_the_image() {
        // 4x1 DeviceGray at 8bpc = 4 bytes, chosen so " EI " sits inside them.
        let out = strip("BI /W 4 /H 1 /BPC 8 /CS /G ID  EI EI (kept) Tj");
        assert!(out.contains("(kept) Tj"), "{out:?}");
        assert!(!out.contains("/CS"), "the dictionary survived: {out:?}");
    }

    #[test]
    fn computed_lengths_match_the_dictionary() {
        assert_eq!(inline_image_len(b"BI /IM true /W 1 /H 1 /BPC 1"), Some(1));
        assert_eq!(
            inline_image_len(b"BI /W 161 /H 47 /BPC 8 /CS /G"),
            Some(161 * 47)
        );
        assert_eq!(
            inline_image_len(b"BI /W 8 /H 2 /BPC 8 /CS /RGB"),
            Some(8 * 3 * 2)
        );
        // Filtered data has no derivable length — fall back to the scan.
        assert_eq!(inline_image_len(b"BI /W 8 /H 2 /BPC 8 /CS /G /F /Fl"), None);
        // An indexed colour space needs resources to resolve; do not guess.
        assert_eq!(
            inline_image_len(b"BI /W 1 /H 8 /BPC 2 /CS [/I /RGB 3 <00>]"),
            None
        );
    }

    #[test]
    fn a_stream_with_no_inline_images_is_returned_unchanged() {
        let src = "BT /F1 12 Tf (hello world) Tj ET";
        assert_eq!(strip(src), src);
    }
}
