//! Shared helpers for the pre-scans over RTF's table destinations.
//!
//! `\fonttbl` and `\stylesheet` are both "definition tables": a group whose direct
//! children each end with a plain-text name terminated by `;`. The body tokenizer
//! skips both, so each is read here in a small independent pass rather than
//! threaded through the tokenizer's group state.

use encoding_rs::Encoding;

use super::encoding::find;

/// Index of the `}` closing the group that opens at `start`.
pub fn group_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2, // skip an escaped brace / control symbol
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Locate a table destination and hand each of its direct child groups to `f`.
pub fn for_each_entry(bytes: &[u8], opener: &[u8], mut f: impl FnMut(&[u8])) {
    let Some(start) = find(bytes, opener) else {
        return;
    };
    let Some(end) = group_end(bytes, start) else {
        return;
    };
    let mut i = start + 1;
    while i < end {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let Some(stop) = group_end(bytes, i) else { break };
        if stop > end {
            break;
        }
        f(&bytes[i..=stop]);
        i = stop + 1;
    }
}

/// Read the plain-text name that terminates a definition: everything after the
/// last control word, up to the `;`. Nested groups (`{\*\falt Arial}`) hold an
/// alternate name, never the real one, so they are skipped entirely.
pub fn read_trailing_name(def: &[u8], enc: &'static Encoding) -> Option<String> {
    let body = def.strip_prefix(b"{").unwrap_or(def);
    let mut raw: Vec<u8> = Vec::new();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < body.len() {
        match body[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                i += 1;
            }
            _ if depth > 0 => i += 1,
            b';' => break,
            b'\\' if i + 1 < body.len() => {
                if body[i + 1] == b'\'' && i + 4 <= body.len() {
                    if let Ok(h) = std::str::from_utf8(&body[i + 2..i + 4]) {
                        if let Ok(b) = u8::from_str_radix(h, 16) {
                            raw.push(b);
                        }
                    }
                    i += 4;
                    continue;
                }
                // A control word means the name has not started yet.
                raw.clear();
                let mut k = i + 1;
                while k < body.len() && body[k].is_ascii_alphabetic() {
                    k += 1;
                }
                while k < body.len() && (body[k].is_ascii_digit() || body[k] == b'-') {
                    k += 1;
                }
                if k < body.len() && body[k] == b' ' {
                    k += 1;
                }
                i = k.max(i + 2);
            }
            c => {
                raw.push(c);
                i += 1;
            }
        }
    }
    let (decoded, _, _) = enc.decode(&raw);
    let name = decoded.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Read the unsigned parameter of `word` where it appears in `def`.
pub fn read_param(def: &[u8], word: &[u8]) -> Option<u32> {
    let pos = find(def, word)?;
    let rest = &def[pos + word.len()..];
    // Guard against matching a longer control word (`\s` inside `\sbasedon`).
    let digits: Vec<u8> = rest.iter().copied().take_while(u8::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(&digits).ok()?.parse().ok()
}
