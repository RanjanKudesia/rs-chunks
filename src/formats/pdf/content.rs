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
    pub width: f32,
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

pub(crate) struct Extractor<'a> {
    doc: &'a Document,
    /// Fonts are shared across pages and parsing one means parsing its ToUnicode
    /// CMap, so a 5,000-page document must not do it 5,000 times.
    fonts: HashMap<ObjectId, Rc<Font>>,
}

impl<'a> Extractor<'a> {
    pub(crate) fn new(doc: &'a Document) -> Self {
        Extractor { doc, fonts: HashMap::new() }
    }

    /// Interpret one content stream against `resources`, appending to `out`.
    pub(crate) fn run(
        &mut self,
        data: &[u8],
        resources: &Dictionary,
        base_ctm: Matrix,
        out: &mut PageContent,
    ) {
        let mut visited = Vec::new();
        self.run_inner(data, resources, base_ctm, out, &mut visited);
    }

    fn run_inner(
        &mut self,
        data: &[u8],
        resources: &Dictionary,
        base_ctm: Matrix,
        out: &mut PageContent,
        visited: &mut Vec<ObjectId>,
    ) {
        let data = strip_inline_images(data);
        let Ok(content) = Content::decode(&data) else { return };
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
                        self.font(resources, &n)
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
                    let Some(items) = a.first().and_then(|o| o.as_array().ok()) else { continue };
                    for item in items {
                        match item {
                            Object::String(s, _) => self.show(&mut ts, ctm, s, out),
                            other => {
                                // A positive number moves left, so the sign is
                                // inverted relative to a translation.
                                let Some(adj) = num(Some(other)) else { continue };
                                let tx = -adj / 1000.0 * ts.size * ts.hscale;
                                ts.tm = Matrix::translate(tx, 0.0).concat(ts.tm);
                            }
                        }
                    }
                }
                "Do" => {
                    if let Some(name) = a.first().and_then(|o| o.as_name().ok()) {
                        self.draw_xobject(resources, name, ctm, out, visited);
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

    fn draw_xobject(
        &mut self,
        resources: &Dictionary,
        name: &[u8],
        ctm: Matrix,
        out: &mut PageContent,
        visited: &mut Vec<ObjectId>,
    ) {
        let Ok(xobjects) = resources.get_deref(b"XObject", self.doc).and_then(Object::as_dict) else {
            return;
        };
        let Ok(entry) = xobjects.get(name) else { return };
        let Ok((id, object)) = self.doc.dereference(entry) else { return };
        let Ok(stream) = object.as_stream() else { return };
        let subtype = stream.dict.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");

        if subtype == b"Image" {
            let Some(id) = id else { return };
            // The image fills the unit square, mapped through the CTM.
            let corners = [ctm.apply(0.0, 0.0), ctm.apply(1.0, 0.0), ctm.apply(0.0, 1.0), ctm.apply(1.0, 1.0)];
            let xs: Vec<f32> = corners.iter().map(|c| c.0).collect();
            let ys: Vec<f32> = corners.iter().map(|c| c.1).collect();
            let (left, right) = (fmin(&xs), fmax(&xs));
            let (bottom, top) = (fmin(&ys), fmax(&ys));
            out.images.push(PlacedImage { id, top, left, width: right - left, height: top - bottom });
            return;
        }
        if subtype != b"Form" {
            return;
        }
        let Some(id) = id else { return };
        if visited.contains(&id) || visited.len() >= MAX_FORM_DEPTH {
            return;
        }
        let Ok(data) = stream.decompressed_content() else { return };
        let form_ctm = match stream.dict.get(b"Matrix").and_then(Object::as_array) {
            Ok(m) => matrix_of(m).unwrap_or(Matrix::IDENTITY).concat(ctm),
            Err(_) => ctm,
        };
        // A form without its own /Resources inherits the caller's.
        let inner = stream
            .dict
            .get_deref(b"Resources", self.doc)
            .and_then(Object::as_dict)
            .cloned()
            .unwrap_or_else(|_| resources.clone());

        visited.push(id);
        self.run_inner(&data, &inner, form_ctm, out, visited);
        visited.pop();
    }

    fn font(&mut self, resources: &Dictionary, name: &[u8]) -> Option<Rc<Font>> {
        let fonts = resources.get_deref(b"Font", self.doc).and_then(Object::as_dict).ok()?;
        let entry = fonts.get(name).ok()?;
        match entry {
            Object::Reference(id) => {
                if let Some(font) = self.fonts.get(id) {
                    return Some(font.clone());
                }
                let dict = self.doc.get_dictionary(*id).ok()?;
                let font = Rc::new(Font::from_dict(self.doc, dict));
                self.fonts.insert(*id, font.clone());
                Some(font)
            }
            Object::Dictionary(dict) => Some(Rc::new(Font::from_dict(self.doc, dict))),
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
        let Some(id) = find_token(data, i, b"ID") else {
            out.extend_from_slice(&data[i..]);
            break;
        };
        // The single whitespace byte after `ID` belongs to the delimiter, and
        // the data begins immediately after it.
        match find_token(data, id + 3, b"EI") {
            Some(end) => i = end + 2,
            None => break,
        }
    }
    out
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
    let v: Vec<f32> = operands.iter().take(6).map(|o| num(Some(o)).unwrap_or(0.0)).collect();
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
