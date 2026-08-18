//! Spec-correct RTF → markdown text extraction (RTF 1.9 / MS-RTF).
//!
//! Hand-rolled because the only Rust RTF crate (`rtf-parser`) mishandles primary
//! `.rtf` content: it leaks `\info` metadata into the body (duplicated), doubles
//! `\uN` unicode with its ASCII fallback, garbles `\'xx` in double-byte codepages
//! (Shift-JIS/GBK/Big5), and panics on invalid unicode. This tokenizer fixes all
//! of those and never panics on malformed input.
//!
//! `\fonttbl`, `\stylesheet` and `\info` are all skipped destinations here; each
//! is read by its own pre-scan (`fonts`, `styles`, `meta`) so the tokenizer stays
//! a single pass over the body.

use encoding_rs::Encoding;

use super::encoding::{codepage_to_encoding, detect_ansicpg};
use super::fonts::{self, Fonts};
use super::lists::marker_for;
use super::meta::{extract_author, extract_title};
use super::styles::{self, HeadingStyles};
use super::writer::{Fmt, Out};

#[derive(Default)]
pub struct RtfDoc {
    pub title: Option<String>,
    pub author: Option<String>,
    pub text: String,
}

#[derive(Clone)]
struct GroupState {
    /// Skip all text in this group (a non-content destination).
    skip: bool,
    /// Number of fallback characters to skip after a `\uN` (from `\ucN`).
    uc_skip: i32,
    /// Encoding for `\'xx` bytes in this group (from the selected font's charset).
    encoding: &'static Encoding,
    /// The selected font paints glyphs, not characters — its bytes are not text.
    symbol_font: bool,
    /// Character formatting in force (`\b`, `\i`).
    fmt: Fmt,
    /// Inside the ANSI half of a `\upr` (unicode-pair) — hard-skip until `\ud`.
    in_upr: bool,
    /// Inside `\info` — a hard skip that `\ud` must not override.
    in_info: bool,
    /// Inside `\listtext` — text is captured as a list marker, not as body.
    in_listtext: bool,
}

/// Tokenizer state that is not per-group.
struct Ctx {
    out: Out,
    /// Captured `\listtext` content for the current list item.
    listtext: String,
    fonts: Fonts,
    heading_styles: HeadingStyles,
    /// Heading level of the paragraph being written, if it is a heading.
    heading_level: Option<u8>,
    uc_pending: i32,
    pending_surrogate: Option<u16>,
}

impl Ctx {
    /// Formatting to actually emit. A heading is already emphatic — wrapping its
    /// text in `**` as well (Word styles headings bold) would put the markers
    /// inside the `#` line for no gain.
    fn fmt(&self, top: &GroupState) -> Fmt {
        if self.heading_level.is_some() {
            Fmt::NONE
        } else {
            top.fmt
        }
    }
}

pub fn extract(bytes: &[u8]) -> RtfDoc {
    let default_enc = detect_ansicpg(bytes);
    let mut doc = RtfDoc {
        title: extract_title(bytes, default_enc),
        author: extract_author(bytes, default_enc),
        text: String::new(),
    };

    let mut stack: Vec<GroupState> = vec![GroupState {
        skip: false,
        uc_skip: 1,
        encoding: default_enc,
        symbol_font: false,
        fmt: Fmt::NONE,
        in_upr: false,
        in_info: false,
        in_listtext: false,
    }];
    let mut ctx = Ctx {
        out: Out::default(),
        listtext: String::new(),
        fonts: fonts::parse(bytes, default_enc),
        heading_styles: styles::parse(bytes, default_enc),
        heading_level: None,
        uc_pending: 0,
        pending_surrogate: None,
    };

    // Raw-byte buffer for consecutive `\'xx` (decoded together for double-byte).
    let mut raw: Vec<u8> = Vec::new();
    let n = bytes.len();
    let mut i = 0usize;

    while i < n {
        match bytes[i] {
            b'{' => {
                let top = stack.last().unwrap().clone();
                flush_raw_at_boundary(&mut raw, &top, &mut ctx);
                stack.push(top);
                i += 1;
            }
            b'}' => {
                let top = stack.last().unwrap().clone();
                flush_raw_at_boundary(&mut raw, &top, &mut ctx);
                if top.in_listtext {
                    end_listtext(&mut ctx);
                }
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            b'\\' => {
                if i + 1 >= n {
                    break;
                }
                let next = bytes[i + 1];
                if next.is_ascii_alphabetic() {
                    let (word, num, consumed) = read_control_word(bytes, i);
                    handle_control_word(word, num, &mut stack, &mut raw, &mut ctx);
                    i = consumed;
                } else if next == b'\'' && i + 4 <= n {
                    if let Some(byte) = read_hex_byte(&bytes[i + 2..i + 4]) {
                        if ctx.uc_pending > 0 {
                            ctx.uc_pending -= 1; // fallback byte after \u — skip
                        } else if writable(stack.last().unwrap()) {
                            raw.push(byte);
                        }
                    }
                    i += 4;
                } else {
                    handle_control_symbol(next, &mut stack, &mut raw, &mut ctx);
                    i += 2;
                }
            }
            b'\r' | b'\n' => {
                i += 1; // raw line breaks are not content in RTF
            }
            c => {
                if ctx.uc_pending > 0 {
                    ctx.uc_pending -= 1; // skip \u fallback char
                } else if writable(stack.last().unwrap()) {
                    raw.push(c);
                }
                i += 1;
            }
        }
    }
    let top = stack.last().unwrap().clone();
    flush_raw(&mut raw, &top, &mut ctx);
    doc.text = ctx.out.finish();
    doc
}

/// Parse `\word`, its optional signed parameter, and the single space delimiter.
fn read_control_word(bytes: &[u8], at: usize) -> (&[u8], Option<i32>, usize) {
    let n = bytes.len();
    let start = at + 1;
    let mut j = start;
    while j < n && bytes[j].is_ascii_alphabetic() {
        j += 1;
    }
    let word = &bytes[start..j];
    let neg = j < n && bytes[j] == b'-';
    let mut k = if neg { j + 1 } else { j };
    let numstart = k;
    while k < n && bytes[k].is_ascii_digit() {
        k += 1;
    }
    let num = if k > numstart {
        let v: i32 = std::str::from_utf8(&bytes[numstart..k])
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        Some(if neg { -v } else { v })
    } else {
        None
    };
    let mut consumed = k;
    if consumed < n && bytes[consumed] == b' ' {
        consumed += 1;
    }
    (word, num, consumed)
}

fn read_hex_byte(hex: &[u8]) -> Option<u8> {
    u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
}

/// Decode the pending `\'xx`/literal bytes and route them to their sink.
fn flush_raw(raw: &mut Vec<u8>, top: &GroupState, ctx: &mut Ctx) {
    if raw.is_empty() {
        return;
    }
    if !writable(top) {
        raw.clear();
        return;
    }
    // Bytes in a pictorial font are glyph indices; decoding them as text is what
    // produced U+FFFD and raw PUA codepoints in list markers.
    if top.symbol_font {
        raw.clear();
        return;
    }
    let (decoded, _, _) = top.encoding.decode(raw);
    if top.in_listtext {
        ctx.listtext.push_str(&decoded);
    } else {
        let fmt = ctx.fmt(top);
        ctx.out.push_text(&decoded, fmt);
    }
    raw.clear();
}

/// Flush at a group boundary, where the formatting and encoding in force are
/// about to change. A skipped group must leave the buffer alone rather than clear
/// it: bytes queued by an enclosing group have to survive a `{\*\htmltag…}` aside
/// that opens and closes inside a run of text, which is how HTML-in-RTF is
/// written and is otherwise most of the document.
fn flush_raw_at_boundary(raw: &mut Vec<u8>, top: &GroupState, ctx: &mut Ctx) {
    if writable(top) {
        flush_raw(raw, top, ctx);
    }
}

/// Whether text in this group reaches a sink at all.
fn writable(top: &GroupState) -> bool {
    !top.skip || top.in_listtext
}

/// Close a `\listtext` group: its glyph becomes a markdown marker.
fn end_listtext(ctx: &mut Ctx) {
    let marker = marker_for(&ctx.listtext);
    ctx.listtext.clear();
    // A heading prefix already queued for this paragraph wins over a marker.
    if !ctx.out.has_line_prefix() {
        ctx.out.set_line_prefix(marker);
    }
}

fn handle_control_symbol(next: u8, stack: &mut [GroupState], raw: &mut Vec<u8>, ctx: &mut Ctx) {
    let top_idx = stack.len() - 1;
    if next == b'*' {
        // `\*` marks a destination a reader may not understand — skip it by
        // default. Without this, `\*\pnseclvlN` numbering templates and style
        // names leak into the body.
        stack[top_idx].skip = true;
        return;
    }
    let top = stack[top_idx].clone();
    match next {
        b'\\' | b'{' | b'}' => push_literal(raw, &top, ctx, next as char),
        b'~' => push_literal(raw, &top, ctx, '\u{00A0}'),
        b'-' => {} // optional hyphen — omit
        b'_' => push_literal(raw, &top, ctx, '-'),
        b'\n' | b'\r' => {
            flush_raw(raw, &top, ctx);
            if !top.skip {
                ctx.out.break_line();
                requeue_heading(ctx);
            }
        }
        _ => {}
    }
}

fn push_literal(raw: &mut Vec<u8>, top: &GroupState, ctx: &mut Ctx, ch: char) {
    flush_raw(raw, top, ctx);
    if top.in_listtext {
        ctx.listtext.push(ch);
    } else if !top.skip {
        let fmt = ctx.fmt(top);
        ctx.out.push_char(ch, fmt);
    }
}

fn handle_control_word(
    word: &[u8],
    num: Option<i32>,
    stack: &mut [GroupState],
    raw: &mut Vec<u8>,
    ctx: &mut Ctx,
) {
    let top_idx = stack.len() - 1;
    let top = stack[top_idx].clone();

    match word {
        // ── Destinations to skip entirely ──
        b"fonttbl"
        | b"colortbl"
        | b"stylesheet"
        | b"listtable"
        | b"listoverridetable"
        | b"revtbl"
        | b"rsidtbl"
        | b"generator"
        | b"themedata"
        | b"colorschememapping"
        | b"latentstyles"
        | b"datastore"
        | b"pict"
        | b"object"
        | b"nonshppict"
        | b"fldinst"
        | b"xmlnstbl"
        | b"mmath"
        | b"header"
        | b"headerl"
        | b"headerr"
        | b"headerf"
        | b"footer"
        | b"footerl"
        | b"footerr"
        | b"footerf" => {
            stack[top_idx].skip = true;
        }
        b"info" => {
            // Skip the entire \info group. Title and author are recovered by the
            // pre-scan in `meta.rs`; capturing them here fights the tokenizer's
            // nested-group handling and risks leaks.
            stack[top_idx].skip = true;
            stack[top_idx].in_info = true;
        }
        // The marker a writer painted for a list item — captured, not emitted.
        b"listtext" => {
            stack[top_idx].skip = true;
            stack[top_idx].in_listtext = true;
        }
        // \upr holds an ANSI copy then a Unicode copy of the same content; skip
        // the ANSI copy (this group), and \ud re-enables the Unicode copy.
        b"upr" => {
            stack[top_idx].skip = true;
            stack[top_idx].in_upr = true;
        }
        // Re-enable the Unicode copy of a \upr — but never inside \info, where
        // the whole group must stay skipped.
        b"ud" if !stack[top_idx].in_info => {
            stack[top_idx].skip = false;
            stack[top_idx].in_upr = false;
        }
        // ── Font selection ──
        b"f" => {
            if let Some(fnum) = num {
                flush_raw(raw, &top, ctx);
                if let Some(font) = ctx.fonts.get(&fnum) {
                    stack[top_idx].encoding = font.encoding;
                    stack[top_idx].symbol_font = font.symbol;
                }
            }
        }
        b"ansicpg" => {
            if let Some(cp) = num {
                stack[top_idx].encoding = codepage_to_encoding(cp as u32);
            }
        }
        b"uc" => {
            if let Some(v) = num {
                stack[top_idx].uc_skip = v.max(0);
            }
        }
        b"u" => {
            if let Some(v) = num {
                flush_raw(raw, &top, ctx);
                push_unicode(v, &top, ctx);
                ctx.uc_pending = stack[top_idx].uc_skip;
            }
        }
        // ── Character formatting ──
        // Text already buffered belongs to the *previous* formatting, so it has
        // to reach the writer before the state changes under it.
        b"b" => {
            flush_raw(raw, &top, ctx);
            stack[top_idx].fmt.bold = num != Some(0);
        }
        b"i" => {
            flush_raw(raw, &top, ctx);
            stack[top_idx].fmt.italic = num != Some(0);
        }
        b"plain" => {
            flush_raw(raw, &top, ctx);
            stack[top_idx].fmt = Fmt::NONE;
        }
        // ── Paragraph style ──
        b"pard" if !top.skip => set_heading(ctx, None),
        b"s" => {
            if let (Some(snum), false) = (num, top.skip) {
                let level = ctx.heading_styles.get(&snum).copied();
                set_heading(ctx, level);
            }
        }
        // ── Structure → text ──
        b"par" | b"line" | b"sect" | b"page" | b"softline" => {
            flush_raw(raw, &top, ctx);
            if !top.skip {
                ctx.out.break_line();
                requeue_heading(ctx);
            }
        }
        b"tab" => {
            flush_raw(raw, &top, ctx);
            if top.in_listtext {
                ctx.listtext.push('\t');
            } else if !top.skip {
                let fmt = ctx.fmt(&top);
                ctx.out.push_text("\t", fmt);
            }
        }
        b"cell" | b"nestcell" => {
            flush_raw(raw, &top, ctx);
            if !top.skip {
                ctx.out.push_structural(" | ");
            }
        }
        b"row" | b"nestrow" => {
            flush_raw(raw, &top, ctx);
            if !top.skip {
                ctx.out.break_line();
            }
        }
        b"bullet" => {
            flush_raw(raw, &top, ctx);
            if top.in_listtext {
                ctx.listtext.push('\u{2022}');
            } else if !top.skip {
                ctx.out.push_structural("- ");
            }
        }
        b"emdash" => push_literal(raw, &top, ctx, '\u{2014}'),
        b"endash" => push_literal(raw, &top, ctx, '\u{2013}'),
        b"lquote" => push_literal(raw, &top, ctx, '\u{2018}'),
        b"rquote" => push_literal(raw, &top, ctx, '\u{2019}'),
        b"ldblquote" => push_literal(raw, &top, ctx, '\u{201C}'),
        b"rdblquote" => push_literal(raw, &top, ctx, '\u{201D}'),
        _ => {} // unknown control word — ignore
    }
}

/// Enter or leave a heading paragraph. Leaving must also retract a marker queued
/// but not yet written, or the `#` lands on the body paragraph that follows —
/// `\par` requeues the marker before the `\pard` that ends the heading arrives.
fn set_heading(ctx: &mut Ctx, level: Option<u8>) {
    let was_heading = ctx.heading_level.is_some();
    ctx.heading_level = level;
    match level {
        Some(l) => ctx
            .out
            .set_line_prefix(format!("{} ", "#".repeat(l as usize))),
        None if was_heading => ctx.out.clear_line_prefix(),
        None => {}
    }
}

/// A heading style stays in force until the next `\pard`, so every paragraph it
/// spans needs its own marker.
fn requeue_heading(ctx: &mut Ctx) {
    if let Some(level) = ctx.heading_level {
        ctx.out
            .set_line_prefix(format!("{} ", "#".repeat(level as usize)));
    }
}

fn push_unicode(v: i32, top: &GroupState, ctx: &mut Ctx) {
    if top.skip && !top.in_listtext {
        return;
    }
    let code = if v < 0 { (v + 65536) as u32 } else { v as u32 };
    if (0xD800..=0xDBFF).contains(&code) {
        // High surrogate — hold for the following low surrogate (e.g. Gothic and
        // other astral characters, which are written as a pair).
        ctx.pending_surrogate = Some(code as u16);
        return;
    }
    let ch = if (0xDC00..=0xDFFF).contains(&code) {
        let Some(hi) = ctx.pending_surrogate.take() else {
            return;
        };
        let combined = 0x10000 + (((hi as u32) - 0xD800) << 10) + (code - 0xDC00);
        match char::from_u32(combined) {
            Some(c) => c,
            None => return,
        }
    } else {
        ctx.pending_surrogate = None;
        char::from_u32(code).unwrap_or('\u{FFFD}')
    };
    // A Private Use codepoint from a pictorial font is a glyph index, not a
    // character — Wingdings writes its bullet as U+F0FC. There is no Unicode
    // character to map it to, so it is dropped rather than leaked as-is.
    if top.symbol_font && (0xE000..=0xF8FF).contains(&code) {
        return;
    }
    if top.in_listtext {
        ctx.listtext.push(ch);
    } else {
        let fmt = ctx.fmt(top);
        ctx.out.push_char(ch, fmt);
    }
}

/// Assemble the extracted document into markdown for the Markdown chunker.
pub fn to_markdown(doc: &RtfDoc) -> String {
    let mut out = String::new();
    if let Some(title) = &doc.title {
        if !title.trim().is_empty() {
            out.push_str(&format!("# {}\n\n", title.trim()));
        }
    }
    out.push_str(&doc.text);
    out.trim().to_string()
}
