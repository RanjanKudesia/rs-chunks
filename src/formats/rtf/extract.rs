//! Spec-correct RTF → markdown text extraction (RTF 1.9 / MS-RTF).
//!
//! Hand-rolled because the only Rust RTF crate (`rtf-parser`) mishandles primary
//! `.rtf` content: it leaks `\info` metadata into the body (duplicated), doubles
//! `\uN` unicode with its ASCII fallback, garbles `\'xx` in double-byte codepages
//! (Shift-JIS/GBK/Big5), and panics on invalid unicode. This tokenizer fixes all
//! of those and never panics on malformed input.

use encoding_rs::{Encoding, BIG5, EUC_KR, GBK, SHIFT_JIS, WINDOWS_1250, WINDOWS_1251,
                  WINDOWS_1252, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255, WINDOWS_1256};

#[derive(Default)]
pub struct RtfDoc {
    pub title: Option<String>,
    pub author: Option<String>,
    pub text: String,
}

/// Map an RTF `\fcharsetN` value to an `encoding_rs` codepage encoding.
fn charset_to_encoding(charset: i32) -> &'static Encoding {
    match charset {
        128 => SHIFT_JIS,   // Japanese
        134 => GBK,         // Simplified Chinese
        136 => BIG5,        // Traditional Chinese
        129 => EUC_KR,      // Korean
        238 => WINDOWS_1250, // Eastern European
        204 => WINDOWS_1251, // Cyrillic
        161 => WINDOWS_1253, // Greek
        162 => WINDOWS_1254, // Turkish
        177 => WINDOWS_1255, // Hebrew
        178 => WINDOWS_1256, // Arabic
        _ => WINDOWS_1252,
    }
}

/// Map an `\ansicpgN` code page number to an encoding.
fn codepage_to_encoding(cp: u32) -> &'static Encoding {
    match cp {
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        _ => WINDOWS_1252,
    }
}

#[derive(Clone)]
struct GroupState {
    /// Skip all text in this group (a non-content destination).
    skip: bool,
    /// Number of fallback characters to skip after a `\uN` (from `\ucN`).
    uc_skip: i32,
    /// Encoding for `\'xx` bytes in this group (from the selected font's charset).
    encoding: &'static Encoding,
    /// Inside the ANSI half of a `\upr` (unicode-pair) — hard-skip until `\ud`.
    in_upr: bool,
    /// Inside `\info` — a hard skip that `\ud` must not override.
    in_info: bool,
}

fn in_fonttbl(stack: &[Special]) -> bool {
    stack.iter().any(|s| *s == Special::FontTable)
}

/// Simple, leak-proof title recovery: read `{\title <plain text>}`. Bails if the
/// title is a nested/`\upr` structure (returns None rather than the ANSI `?????`
/// fallback copy), decoding `\'xx`/`\uN`/escapes in the default encoding.
fn extract_title(bytes: &[u8], enc: &'static Encoding) -> Option<String> {
    let pos = find(bytes, b"{\\title")?;
    let mut i = pos + 7;
    if i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    let mut raw: Vec<u8> = Vec::new();
    let mut out = String::new();
    let flush = |raw: &mut Vec<u8>, out: &mut String| {
        if !raw.is_empty() {
            let (d, _, _) = enc.decode(raw);
            out.push_str(&d);
            raw.clear();
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b'{' => return None, // nested (\upr etc.) — don't risk the wrong copy
            b'}' => break,
            b'\\' if i + 1 < bytes.len() => {
                let nx = bytes[i + 1];
                if nx == b'\'' && i + 4 <= bytes.len() {
                    if let Ok(h) = std::str::from_utf8(&bytes[i + 2..i + 4]) {
                        if let Ok(b) = u8::from_str_radix(h, 16) {
                            raw.push(b);
                        }
                    }
                    i += 4;
                    continue;
                } else if nx == b'u' && bytes.get(i + 2).is_some_and(|c| c.is_ascii_digit() || *c == b'-') {
                    flush(&mut raw, &mut out);
                    let mut k = i + 2;
                    let neg = bytes[k] == b'-';
                    if neg {
                        k += 1;
                    }
                    let st = k;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    let v: i32 = std::str::from_utf8(&bytes[st..k]).unwrap_or("0").parse().unwrap_or(0);
                    let code = if neg { (-v + 65536) as u32 } else { v as u32 };
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                    i = k;
                    continue;
                } else if matches!(nx, b'\\' | b'{' | b'}') {
                    flush(&mut raw, &mut out);
                    out.push(nx as char);
                    i += 2;
                    continue;
                }
                // other control word — skip letters + optional number + space
                let mut k = i + 1;
                while k < bytes.len() && bytes[k].is_ascii_alphabetic() {
                    k += 1;
                }
                while k < bytes.len() && (bytes[k].is_ascii_digit() || bytes[k] == b'-') {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b' ' {
                    k += 1;
                }
                flush(&mut raw, &mut out);
                i = k;
            }
            c => {
                raw.push(c);
                i += 1;
            }
        }
    }
    flush(&mut raw, &mut out);
    let t = out.trim().to_string();
    // Discard an all-`?` ANSI \upr fallback title (the real one is the \ud copy).
    if t.is_empty() || t.chars().all(|c| c == '?' || c.is_whitespace()) {
        None
    } else {
        Some(t)
    }
}

/// A special destination we handle rather than blindly skip.
#[derive(Clone, Copy, PartialEq)]
enum Special {
    None,
    FontTable,
}

pub fn extract(bytes: &[u8]) -> RtfDoc {
    let mut doc = RtfDoc::default();
    let default_enc = detect_ansicpg(bytes);
    doc.title = extract_title(bytes, default_enc);

    let mut stack: Vec<GroupState> = vec![GroupState {
        skip: false,
        uc_skip: 1,
        encoding: default_enc,
        in_upr: false,
        in_info: false,
    }];
    let mut special_stack: Vec<Special> = vec![Special::None];

    // Font-table charset capture: fN → encoding, applied when `\fN` is selected.
    let mut font_enc: std::collections::HashMap<i32, &'static Encoding> = std::collections::HashMap::new();
    let mut current_font_in_fonttbl: Option<i32> = None;

    // Raw-byte buffer for consecutive `\'xx` (decoded together for double-byte).
    let mut raw: Vec<u8> = Vec::new();
    // Where captured text goes (body, or a metadata field).
    let mut uc_pending = 0i32;
    let mut pending_surrogate: Option<u16> = None;

    let n = bytes.len();
    let mut i = 0usize;

    macro_rules! cur {
        () => {
            stack.last().unwrap()
        };
    }
    macro_rules! flush_raw {
        ($sink:expr) => {
            if !raw.is_empty() {
                let (decoded, _, _) = cur!().encoding.decode(&raw);
                $sink.push_str(&decoded);
                raw.clear();
            }
        };
    }

    while i < n {
        let c = bytes[i];
        // Title/author metadata are recovered by the `extract_title` pre-scan;
        // the body never captures metadata, so this is always false. Kept as a
        // named constant so the emit paths below read clearly.
        let capturing_meta = false;

        match c {
            b'{' => {
                let top = cur!().clone();
                stack.push(top);
                special_stack.push(Special::None);
                i += 1;
            }
            b'}' => {
                // Closing a group — flush any pending raw bytes into the body.
                if !cur!().skip {
                    flush_raw!(doc.text);
                }
                if stack.len() > 1 {
                    stack.pop();
                    special_stack.pop();
                }
                current_font_in_fonttbl = None;
                i += 1;
            }
            b'\\' => {
                if i + 1 >= n {
                    break;
                }
                let next = bytes[i + 1];
                if next.is_ascii_alphabetic() {
                    // Control word: letters + optional signed number + optional space.
                    let start = i + 1;
                    let mut j = start;
                    while j < n && bytes[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    let word = &bytes[start..j];
                    // optional numeric parameter (signed)
                    let mut num: Option<i32> = None;
                    let neg = j < n && bytes[j] == b'-';
                    let mut k = if neg { j + 1 } else { j };
                    let numstart = k;
                    while k < n && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k > numstart {
                        let s = std::str::from_utf8(&bytes[numstart..k]).unwrap_or("0");
                        let v: i32 = s.parse().unwrap_or(0);
                        num = Some(if neg { -v } else { v });
                    }
                    // consume a single trailing space delimiter
                    let mut consumed = k;
                    if consumed < n && bytes[consumed] == b' ' {
                        consumed += 1;
                    }
                    handle_control_word(
                        word,
                        num,
                        &mut stack,
                        &mut special_stack,
                        &mut font_enc,
                        &mut current_font_in_fonttbl,
                        &mut raw,
                        &mut doc.text,
                        &mut uc_pending,
                        &mut pending_surrogate,
                        capturing_meta,
                        default_enc,
                    );
                    i = consumed;
                } else if next == b'\'' {
                    // \'xx hex byte.
                    if i + 3 < n + 1 && i + 4 <= n {
                        let hex = &bytes[i + 2..(i + 4).min(n)];
                        if hex.len() == 2 {
                            if let Ok(s) = std::str::from_utf8(hex) {
                                if let Ok(byte) = u8::from_str_radix(s, 16) {
                                    if uc_pending > 0 {
                                        uc_pending -= 1; // fallback byte after \u — skip
                                    } else if !cur!().skip && !capturing_meta {
                                        raw.push(byte);
                                    } else if capturing_meta {
                                        raw.push(byte);
                                    }
                                }
                            }
                            i += 4;
                            continue;
                        }
                    }
                    i += 2;
                } else {
                    // Control symbol.
                    match next {
                        b'*' => {
                            // \*  → the following destination is skippable unless
                            // we recognise it; mark current group's next dest skip.
                            // Handled when the destination control word arrives; here
                            // just note by setting skip on the current group only if
                            // no special handler claims it.
                            special_stack.pop();
                            special_stack.push(Special::None);
                            if let Some(top) = stack.last_mut() {
                                top.skip = true; // default: skip \*\... destinations
                            }
                        }
                        b'\\' | b'{' | b'}' => {
                            if !cur!().skip {
                                flush_raw!(doc.text);
                                doc.text.push(next as char);
                            }
                        }
                        b'~' => {
                            if !cur!().skip && !capturing_meta {
                                flush_raw!(doc.text);
                                doc.text.push('\u{00A0}');
                            }
                        }
                        b'-' => {} // optional hyphen — omit
                        b'_' => {
                            if !cur!().skip && !capturing_meta {
                                flush_raw!(doc.text);
                                doc.text.push('-');
                            }
                        }
                        b'\n' | b'\r' => {
                            if !cur!().skip && !capturing_meta {
                                flush_raw!(doc.text);
                                doc.text.push('\n');
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
            }
            b'\r' | b'\n' => {
                i += 1; // raw line breaks are not content in RTF
            }
            _ => {
                // Literal text byte.
                if uc_pending > 0 {
                    uc_pending -= 1; // skip \u fallback char
                } else if !cur!().skip {
                    raw.push(c);
                }
                // else: inside a skipped destination (fonttbl/info/…) — ignored.
                i += 1;
            }
        }
    }
    // Final flush.
    if !raw.is_empty() {
        let (decoded, _, _) = stack.last().unwrap().encoding.decode(&raw);
        doc.text.push_str(&decoded);
    }
    doc.text = normalize(&doc.text);
    doc
}

/// Find `\ansicpgN` near the header to set the default `\'xx` encoding.
fn detect_ansicpg(bytes: &[u8]) -> &'static Encoding {
    let scan = &bytes[..bytes.len().min(512)];
    if let Some(pos) = find(scan, b"\\ansicpg") {
        let mut k = pos + 8;
        let mut num = 0u32;
        while k < scan.len() && scan[k].is_ascii_digit() {
            num = num * 10 + (scan[k] - b'0') as u32;
            k += 1;
        }
        if num > 0 {
            return codepage_to_encoding(num);
        }
    }
    WINDOWS_1252
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[allow(clippy::too_many_arguments)]
fn handle_control_word(
    word: &[u8],
    num: Option<i32>,
    stack: &mut [GroupState],
    special_stack: &mut Vec<Special>,
    font_enc: &mut std::collections::HashMap<i32, &'static Encoding>,
    current_font_in_fonttbl: &mut Option<i32>,
    raw: &mut Vec<u8>,
    text: &mut String,
    uc_pending: &mut i32,
    pending_surrogate: &mut Option<u16>,
    capturing_meta: bool,
    default_enc: &'static Encoding,
) {
    let top_idx = stack.len() - 1;
    macro_rules! flush {
        () => {
            if !raw.is_empty() && !stack[top_idx].skip && !capturing_meta {
                let (d, _, _) = stack[top_idx].encoding.decode(raw);
                text.push_str(&d);
                raw.clear();
            } else {
                raw.clear();
            }
        };
    }

    match word {
        // ── Destinations to skip entirely ──
        b"fonttbl" => {
            stack[top_idx].skip = true;
            *special_stack.last_mut().unwrap() = Special::FontTable;
        }
        b"colortbl" | b"stylesheet" | b"listtable" | b"listoverridetable" | b"revtbl"
        | b"rsidtbl" | b"generator" | b"themedata" | b"colorschememapping"
        | b"latentstyles" | b"datastore" | b"pict" | b"object" | b"nonshppict"
        | b"fldinst" | b"xmlnstbl" | b"mmath" | b"header" | b"headerl" | b"headerr"
        | b"headerf" | b"footer" | b"footerl" | b"footerr" | b"footerf" => {
            stack[top_idx].skip = true;
        }
        b"info" => {
            // Skip the entire \info group. Title is recovered separately by a
            // simple pre-scan (see `extract_title`); capturing title/author here
            // fights the tokenizer's nested-group handling and risks leaks.
            stack[top_idx].skip = true;
            stack[top_idx].in_info = true;
        }
        // \upr holds an ANSI copy then a Unicode copy of the same content; skip
        // the ANSI copy (this group), and \ud re-enables the Unicode copy.
        b"upr" => {
            stack[top_idx].skip = true;
            stack[top_idx].in_upr = true;
        }
        b"ud" => {
            // Re-enable the Unicode copy of a \upr — but never inside \info,
            // where the whole group must stay skipped.
            if !stack[top_idx].in_info {
                stack[top_idx].skip = false;
                stack[top_idx].in_upr = false;
            }
        }
        // ── Font table charset capture ──
        b"f" => {
            if let Some(fnum) = num {
                if in_fonttbl(special_stack) {
                    *current_font_in_fonttbl = Some(fnum);
                } else if let Some(enc) = font_enc.get(&fnum) {
                    flush!();
                    stack[top_idx].encoding = enc;
                }
            }
        }
        b"fcharset" => {
            if let (Some(charset), Some(fnum)) = (num, *current_font_in_fonttbl) {
                font_enc.insert(fnum, charset_to_encoding(charset));
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
                let code = if v < 0 { (v + 65536) as u32 } else { v as u32 };
                if !stack[top_idx].skip && !capturing_meta {
                    flush!();
                    if (0xD800..=0xDBFF).contains(&code) {
                        // High surrogate — hold for the following low surrogate
                        // (e.g. Gothic/astral characters split into a pair).
                        *pending_surrogate = Some(code as u16);
                    } else if (0xDC00..=0xDFFF).contains(&code) {
                        if let Some(hi) = pending_surrogate.take() {
                            let combined = 0x10000
                                + (((hi as u32) - 0xD800) << 10)
                                + (code - 0xDC00);
                            if let Some(ch) = char::from_u32(combined) {
                                text.push(ch);
                            }
                        }
                    } else if let Some(ch) = char::from_u32(code) {
                        *pending_surrogate = None;
                        text.push(ch);
                    } else {
                        *pending_surrogate = None;
                        text.push('\u{FFFD}');
                    }
                }
                *uc_pending = stack[top_idx].uc_skip;
            }
        }
        // ── Structure → text ──
        b"par" | b"line" | b"sect" | b"page" | b"softline" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push('\n');
            }
        }
        b"tab" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push('\t');
            }
        }
        b"cell" | b"nestcell" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push_str(" | ");
            }
        }
        b"row" | b"nestrow" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push('\n');
            }
        }
        b"bullet" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push_str("- ");
            }
        }
        b"emdash" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push('\u{2014}');
            }
        }
        b"endash" => {
            flush!();
            if !stack[top_idx].skip && !capturing_meta {
                text.push('\u{2013}');
            }
        }
        b"lquote" => push_ch(stack, top_idx, capturing_meta, raw, text, '\u{2018}'),
        b"rquote" => push_ch(stack, top_idx, capturing_meta, raw, text, '\u{2019}'),
        b"ldblquote" => push_ch(stack, top_idx, capturing_meta, raw, text, '\u{201C}'),
        b"rdblquote" => push_ch(stack, top_idx, capturing_meta, raw, text, '\u{201D}'),
        _ => {
            // Unknown control word — ignore. `default_enc` unused branch guard.
            let _ = default_enc;
        }
    }
}

fn push_ch(
    stack: &mut [GroupState],
    top_idx: usize,
    capturing_meta: bool,
    raw: &mut Vec<u8>,
    text: &mut String,
    ch: char,
) {
    if !raw.is_empty() && !stack[top_idx].skip && !capturing_meta {
        let (d, _, _) = stack[top_idx].encoding.decode(raw);
        text.push_str(&d);
    }
    raw.clear();
    if !stack[top_idx].skip && !capturing_meta {
        text.push(ch);
    }
}

/// Collapse excess blank lines / trailing whitespace into clean markdown-ish text.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.split('\n') {
        let t = line.trim_end();
        if t.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(t.trim_start_matches(|c| c == ' '));
            out.push('\n');
        }
    }
    out.trim().to_string()
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
