//! A Type 1 font program's *built-in* encoding.
//!
//! A font dictionary may name no `/Encoding` and carry no `/ToUnicode`, leaving
//! the font program itself as the only statement of what a code means. For Type
//! 1 that statement is in the clear: the program opens with an ASCII header
//! holding an `/Encoding` array written as `dup <code> /<name> put`, and only
//! the CharStrings after `eexec` are encrypted.
//!
//! This matters most for TeX's Computer Modern maths fonts, which are marked
//! symbolic, name no encoding, and place `/alpha`, `/arrowright` and the italic
//! letters at codes of their own choosing. Reading the header recovers them
//! exactly; guessing a standard encoding would put the wrong letters there.

use super::font::glyph_to_char;

/// Only the header is scanned. `eexec` marks where the encrypted part begins,
/// and this cap bounds the search on a font that has no header at all.
const MAX_HEADER: usize = 64_000;

/// Read a Type 1 program's `/Encoding`, or `None` if it names a standard one or
/// has none to read.
pub(crate) fn builtin_encoding(program: &[u8]) -> Option<[Option<char>; 256]> {
    let end = find(program, b"eexec")
        .unwrap_or(program.len())
        .min(MAX_HEADER);
    let header = &program[..end];
    let start = find(header, b"/Encoding")? + "/Encoding".len();
    let header = &header[start..];

    // `/Encoding StandardEncoding def` defers to the standard table, which the
    // caller already has; only an inline array carries new information.
    if header.trim_ascii_start().starts_with(b"StandardEncoding") {
        return None;
    }

    let mut table: [Option<char>; 256] = [None; 256];
    let mut found = false;
    let mut i = 0;
    while let Some(offset) = find(&header[i..], b"dup ") {
        let at = i + offset + 4;
        i = at;
        let Some((code, rest)) = read_code(&header[at..]) else {
            continue;
        };
        let Some(name) = read_name(rest) else {
            continue;
        };
        if code < 256 {
            table[code] = glyph_to_char(&name);
            found = true;
        }
        // `def` closes the array; anything after it is a different construct.
        if let Some(stop) = find(&header[at..], b" def") {
            if stop < offset.max(1) {
                break;
            }
        }
    }
    found.then_some(table)
}

fn read_code(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let code = std::str::from_utf8(&bytes[..digits]).ok()?.parse().ok()?;
    Some((code, &bytes[digits..]))
}

fn read_name(bytes: &[u8]) -> Option<String> {
    let start = bytes.iter().position(|b| *b == b'/')? + 1;
    // A name runs to the next delimiter; `put` follows it.
    let len = bytes[start..]
        .iter()
        .take_while(|b| !b.is_ascii_whitespace() && !b"()<>[]{}/%".contains(b))
        .count();
    if len == 0 || start > 4 {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..start + len]).to_string())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape pdfTeX writes for a Computer Modern maths font.
    const CM_HEADER: &[u8] = b"%!PS-AdobeFont-1.0: CMMI10\n\
        /FontName /CMMI10 def\n\
        /Encoding 256 array\n\
        0 1 255 {1 index exch /.notdef put} for\n\
        dup 75 /K put\n\
        dup 11 /alpha put\n\
        dup 12 /beta put\n\
        readonly def\n\
        currentdict end\ncurrentfile eexec \x80\x01\x02\x03";

    #[test]
    fn a_built_in_encoding_is_read_from_the_clear_header() {
        let table = builtin_encoding(CM_HEADER).expect("an inline encoding");
        assert_eq!(table[75], Some('K'));
        assert_eq!(table[11], Some('α'));
        assert_eq!(table[12], Some('β'));
        assert_eq!(table[13], None);
    }

    #[test]
    fn a_font_deferring_to_the_standard_encoding_reports_none() {
        assert!(
            builtin_encoding(b"/FontName /X def\n/Encoding StandardEncoding def\neexec").is_none()
        );
    }

    #[test]
    fn a_program_without_an_encoding_reports_none() {
        assert!(builtin_encoding(b"%!PS-AdobeFont-1.0\n/FontName /X def\neexec\x00\x01").is_none());
    }

    #[test]
    fn the_encrypted_body_is_never_scanned() {
        // `dup 65 /Z put` after eexec is ciphertext that happens to look like an
        // encoding entry, and must not be read as one.
        let mut program = CM_HEADER.to_vec();
        program.extend_from_slice(b"dup 65 /Z put");
        assert_eq!(builtin_encoding(&program).unwrap()[65], None);
    }
}
