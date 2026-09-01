//! `\info` metadata recovery by pre-scan.
//!
//! The body tokenizer skips `\info` wholesale — capturing fields there fights its
//! nested-group handling and has leaked `\info` text into the body before. Each
//! field is instead recovered by a small, self-contained scan of `{\field …}`.

use encoding_rs::Encoding;

use super::encoding::find;

/// Read `{\title …}`.
pub fn extract_title(bytes: &[u8], enc: &'static Encoding) -> Option<String> {
    extract_field(bytes, b"{\\title", enc)
}

/// Read `{\author …}`.
///
/// Where a writer emits a `\upr` pair, the first `{\author …}` is the ANSI copy.
/// That is deliberately the one taken: the `\*\ud` Unicode copy is a nested group
/// this scan will not descend into, and in at least one real fixture
/// (`tika_testRTFListMicrosoftWord.rtf`) that copy is itself corrupt — it encodes
/// `ö` as `\u-7014`, a Private Use codepoint. The ANSI copy is lossy in the same
/// file (`Axel D?fler`) but is what the document actually asserts.
pub fn extract_author(bytes: &[u8], enc: &'static Encoding) -> Option<String> {
    extract_field(bytes, b"{\\author", enc)
}

/// Read a flat `{\keyword <plain text>}` field. Bails to `None` if the value is a
/// nested/`\upr` structure rather than risking the wrong copy, decoding
/// `\'xx`/`\uN`/escapes in the default encoding.
fn extract_field(bytes: &[u8], opener: &[u8], enc: &'static Encoding) -> Option<String> {
    let mut from = 0usize;
    loop {
        let pos = from + find(&bytes[from..], opener)?;
        from = pos + opener.len();
        // The control word must end here, or this is a longer word that merely
        // starts the same way (`{\title` vs `{\titlepg`) — keep looking. Only a
        // false match resumes the search: once the real field is found, its value
        // is the answer, even if that answer is "unrecoverable".
        if bytes.get(from).is_some_and(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        return read_field_value(bytes, from, enc);
    }
}

/// Decode a field value starting just past its control word, up to the group end.
fn read_field_value(bytes: &[u8], start: usize, enc: &'static Encoding) -> Option<String> {
    let mut i = start;
    if bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    let mut raw: Vec<u8> = Vec::new();
    let mut out = String::new();
    // Each `\uN` is followed by `\ucN` fallback characters for readers that
    // cannot handle Unicode. Skipping them is what stops a CJK value coming back
    // as `ゾ?ル?ゲ?`.
    let mut uc_skip = 1i32;
    let mut uc_pending = 0i32;
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
                    if uc_pending > 0 {
                        uc_pending -= 1;
                    } else if let Ok(h) = std::str::from_utf8(&bytes[i + 2..i + 4]) {
                        if let Ok(b) = u8::from_str_radix(h, 16) {
                            raw.push(b);
                        }
                    }
                    i += 4;
                    continue;
                } else if nx == b'u'
                    && bytes
                        .get(i + 2)
                        .is_some_and(|c| c.is_ascii_digit() || *c == b'-')
                {
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
                    let v: i32 = std::str::from_utf8(&bytes[st..k])
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0);
                    let code = if neg { (-v + 65536) as u32 } else { v as u32 };
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                    uc_pending = uc_skip;
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
                let wstart = k;
                while k < bytes.len() && bytes[k].is_ascii_alphabetic() {
                    k += 1;
                }
                let wend = k;
                while k < bytes.len() && (bytes[k].is_ascii_digit() || bytes[k] == b'-') {
                    k += 1;
                }
                if &bytes[wstart..wend] == b"uc" {
                    uc_skip = std::str::from_utf8(&bytes[wend..k])
                        .ok()
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(uc_skip)
                        .max(0);
                }
                if k < bytes.len() && bytes[k] == b' ' {
                    k += 1;
                }
                flush(&mut raw, &mut out);
                i = k;
            }
            c => {
                if uc_pending > 0 {
                    uc_pending -= 1;
                } else {
                    raw.push(c);
                }
                i += 1;
            }
        }
    }
    flush(&mut raw, &mut out);
    let t = out.trim().to_string();
    // Discard an all-`?` ANSI \upr fallback (the real value is the \ud copy).
    if t.is_empty() || t.chars().all(|c| c == '?' || c.is_whitespace()) {
        None
    } else {
        Some(t)
    }
}
