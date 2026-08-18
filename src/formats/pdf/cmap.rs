//! `/ToUnicode` CMap parsing: character code → the text it stands for.
//!
//! A ToUnicode CMap is a small PostScript program, but only two of its
//! constructs carry the mapping, so it is read as a token stream rather than
//! interpreted:
//!
//! ```text
//! 2 beginbfchar  <03> <0020>  <04> <0041>  endbfchar
//! 1 beginbfrange <05> <07> <0043>  <08> <0A> [<44> <45> <46>]  endbfrange
//! ```
//!
//! Sources are hex codes (their byte width is the font's code width); targets
//! are UTF-16BE, and may be multi-character — an `ffi` ligature maps one code to
//! three characters, which is why the map's value is a `String`.

use std::collections::HashMap;

/// Refuse to expand an implausible bfrange. A corrupt or hostile CMap can
/// declare `<0000> <FFFFFFFF>`; the cap keeps that from becoming an OOM.
const MAX_RANGE: u32 = 65_536;

#[derive(Debug, PartialEq)]
enum Token {
    Hex(Vec<u8>),
    Keyword(String),
    ArrayStart,
    ArrayEnd,
    Other,
}

pub(crate) fn parse_to_unicode(bytes: &[u8]) -> HashMap<u32, String> {
    let tokens = tokenize(bytes);
    let mut map = HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Keyword(k) if k == "beginbfchar" => i = read_bfchar(&tokens, i + 1, &mut map),
            Token::Keyword(k) if k == "beginbfrange" => i = read_bfrange(&tokens, i + 1, &mut map),
            _ => i += 1,
        }
    }
    map
}

fn read_bfchar(tokens: &[Token], mut i: usize, map: &mut HashMap<u32, String>) -> usize {
    while i + 1 < tokens.len() {
        let (Token::Hex(src), Token::Hex(dst)) = (&tokens[i], &tokens[i + 1]) else {
            return i + 1;
        };
        insert(map, code_of(src), dst);
        i += 2;
    }
    i
}

fn read_bfrange(tokens: &[Token], mut i: usize, map: &mut HashMap<u32, String>) -> usize {
    while i + 2 < tokens.len() {
        let (Token::Hex(lo), Token::Hex(hi)) = (&tokens[i], &tokens[i + 1]) else {
            return i + 1;
        };
        let (lo, hi) = (code_of(lo), code_of(hi));
        if hi < lo || hi - lo >= MAX_RANGE {
            return i + 2;
        }
        match &tokens[i + 2] {
            // `<lo> <hi> <dst>` — dst increments with the code. Only the last
            // UTF-16 unit advances, which is what the spec prescribes.
            Token::Hex(dst) => {
                for code in lo..=hi {
                    insert_offset(map, code, dst, code - lo);
                }
                i += 3;
            }
            // `<lo> <hi> [ <dst> <dst> … ]` — one explicit target per code.
            Token::ArrayStart => {
                let mut code = lo;
                let mut k = i + 3;
                while k < tokens.len() {
                    match &tokens[k] {
                        Token::Hex(dst) => {
                            if code <= hi {
                                insert(map, code, dst);
                            }
                            code += 1;
                            k += 1;
                        }
                        _ => break,
                    }
                }
                i = if matches!(tokens.get(k), Some(Token::ArrayEnd)) {
                    k + 1
                } else {
                    k
                };
            }
            _ => return i + 3,
        }
    }
    i
}

fn insert(map: &mut HashMap<u32, String>, code: u32, dst: &[u8]) {
    let text = utf16be(dst);
    if !text.is_empty() {
        map.insert(code, text);
    }
}

fn insert_offset(map: &mut HashMap<u32, String>, code: u32, dst: &[u8], offset: u32) {
    let mut units: Vec<u16> = dst
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    if units.is_empty() {
        return;
    }
    let last = units.len() - 1;
    units[last] = units[last].wrapping_add(offset as u16);
    let text = String::from_utf16_lossy(&units);
    if !text.is_empty() {
        map.insert(code, text);
    }
}

fn utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units).replace('\u{0}', "")
}

/// A CMap source code is a big-endian integer over its hex string's bytes.
fn code_of(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .take(4)
        .fold(0u32, |acc, b| (acc << 8) | *b as u32)
}

fn tokenize(bytes: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'%' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'<' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'>' {
                    end += 1;
                }
                out.push(Token::Hex(hex_bytes(&bytes[start..end.min(bytes.len())])));
                i = end + 1;
            }
            b'[' => {
                out.push(Token::ArrayStart);
                i += 1;
            }
            b']' => {
                out.push(Token::ArrayEnd);
                i += 1;
            }
            b if b.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.push(Token::Keyword(
                    String::from_utf8_lossy(&bytes[start..i]).to_string(),
                ));
            }
            // Whitespace separates tokens without being one: the readers below
            // require a mapping's source and target to be *adjacent*.
            b if b.is_ascii_whitespace() => i += 1,
            _ => {
                out.push(Token::Other);
                i += 1;
            }
        }
    }
    out
}

/// Hex digits to bytes, ignoring whitespace. An odd trailing digit is padded on
/// the right, matching how PDF hex strings are defined.
fn hex_bytes(digits: &[u8]) -> Vec<u8> {
    let mut nibbles: Vec<u8> = Vec::with_capacity(digits.len());
    for d in digits {
        match d {
            b'0'..=b'9' => nibbles.push(d - b'0'),
            b'a'..=b'f' => nibbles.push(d - b'a' + 10),
            b'A'..=b'F' => nibbles.push(d - b'A' + 10),
            _ => {}
        }
    }
    if nibbles.len() % 2 == 1 {
        nibbles.push(0);
    }
    nibbles
        .chunks_exact(2)
        .map(|c| (c[0] << 4) | c[1])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfchar_and_bfrange_are_both_read() {
        let src = b"2 beginbfchar\n<03> <0020>\n<04> <0041>\nendbfchar\n\
                    1 beginbfrange\n<05> <07> <0043>\nendbfrange";
        let map = parse_to_unicode(src);
        assert_eq!(map[&3], " ");
        assert_eq!(map[&4], "A");
        assert_eq!(map[&5], "C");
        assert_eq!(map[&6], "D");
        assert_eq!(map[&7], "E");
    }

    #[test]
    fn a_ligature_maps_one_code_to_several_characters() {
        let map = parse_to_unicode(b"1 beginbfchar <01> <00660066006C> endbfchar");
        assert_eq!(map[&1], "ffl");
    }

    #[test]
    fn an_array_bfrange_assigns_one_target_per_code() {
        let map = parse_to_unicode(b"1 beginbfrange <08> <0A> [<0044> <0045> <0046>] endbfrange");
        assert_eq!(
            (map[&8].as_str(), map[&9].as_str(), map[&10].as_str()),
            ("D", "E", "F")
        );
    }

    #[test]
    fn two_byte_codes_keep_their_width() {
        let map = parse_to_unicode(b"1 beginbfchar <0103> <0041> endbfchar");
        assert_eq!(map[&0x0103], "A");
        assert!(!map.contains_key(&1));
    }

    #[test]
    fn an_absurd_range_is_refused_rather_than_expanded() {
        let map = parse_to_unicode(b"1 beginbfrange <0000> <FFFFFF> <0041> endbfrange");
        assert!(map.is_empty());
    }
}
