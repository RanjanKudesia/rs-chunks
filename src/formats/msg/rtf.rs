//! RTF handling for the `.msg` body fallback.
//!
//! `PidTagRtfCompressed` (0x1009) stores the body as LZFu-compressed RTF
//! (MS-OXRTFCP). We decompress it here, then hand the RTF to the engine's own
//! reader in [`crate::formats::rtf`] — which already skips Outlook's
//! `\*\htmltag` destinations for `\fromhtml` encapsulated HTML, *and* decodes
//! `\'xx` escapes against the document's codepage, which the `rtf-parser` crate
//! did not (TECH_DEBT #49).

/// The 207-byte preset dictionary every LZFu stream starts from (MS-OXRTFCP).
const LZFU_INIT_DICT: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

const MAGIC_COMPRESSED: u32 = 0x7546_5A4C; // "LZFu"
const MAGIC_UNCOMPRESSED: u32 = 0x414C_454D; // "MELA"
const DICT_SIZE: usize = 4096;

fn u32_le(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

/// Decompress an LZFu-compressed RTF stream (`PidTagRtfCompressed`).
pub fn decompress_rtf(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 16 {
        return Err("Compressed RTF stream too short".to_string());
    }
    let raw_size = u32_le(data, 4) as usize;
    let magic = u32_le(data, 8);
    let body = &data[16..];

    if magic == MAGIC_UNCOMPRESSED {
        let end = raw_size.min(body.len());
        return Ok(body[..end].to_vec());
    }
    if magic != MAGIC_COMPRESSED {
        return Err(format!("Unknown compressed-RTF magic 0x{magic:08X}"));
    }

    let mut dict = vec![0u8; DICT_SIZE];
    dict[..LZFU_INIT_DICT.len()].copy_from_slice(LZFU_INIT_DICT);
    let mut write_pos = LZFU_INIT_DICT.len(); // 207
    let mut out = Vec::with_capacity(raw_size);
    let mut ip = 0usize;

    'outer: while ip < body.len() {
        let flags = body[ip];
        ip += 1;
        for bit in 0..8 {
            if (flags >> bit) & 1 == 0 {
                // Literal byte.
                if ip >= body.len() {
                    break 'outer;
                }
                let b = body[ip];
                ip += 1;
                out.push(b);
                dict[write_pos % DICT_SIZE] = b;
                write_pos = (write_pos + 1) % DICT_SIZE;
            } else {
                // Dictionary reference: 12-bit offset + 4-bit (length-2).
                if ip + 1 >= body.len() {
                    break 'outer;
                }
                let b1 = body[ip] as usize;
                let b2 = body[ip + 1] as usize;
                ip += 2;
                let offset = (b1 << 4) | (b2 >> 4);
                let length = (b2 & 0x0F) + 2;
                if offset == write_pos {
                    break 'outer; // end-of-stream marker
                }
                for i in 0..length {
                    let b = dict[(offset + i) % DICT_SIZE];
                    out.push(b);
                    dict[write_pos % DICT_SIZE] = b;
                    write_pos = (write_pos + 1) % DICT_SIZE;
                }
            }
            if out.len() >= raw_size {
                break 'outer;
            }
        }
    }

    out.truncate(raw_size);
    Ok(out)
}

/// Convert RTF bytes to plain text, using the engine's own RTF reader.
///
/// This used to hand the bytes to the `rtf-parser` crate, which does not decode
/// `\'xx` escapes against the document's codepage. A `\ansicpg950` message body
/// is 751 such escapes and came back **empty** — `.msg` silently lost every
/// non-Latin RTF body it had (TECH_DEBT #49).
///
/// `formats::rtf` already does this properly: it reads `\ansicpg` and each
/// font's `\fcharset`, and batches `\'xx` bytes so a double-byte pair decodes
/// together. Keeping a second, weaker RTF reader inside `msg` was the drift the
/// one-engine rule exists to prevent.
pub fn rtf_to_text(rtf_bytes: &[u8]) -> String {
    crate::formats::rtf::to_markdown_from_bytes(rtf_bytes).unwrap_or_default()
}

/// Collapse the whitespace runs that Outlook's encapsulated-HTML RTF leaves
/// between table cells (`\r\t\r …`), while preserving paragraph breaks.
fn normalize_whitespace(text: &str) -> String {
    text.split("\n\n")
        .map(|para| para.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Full pipeline: decompress `PidTagRtfCompressed` bytes → normalized text.
pub fn compressed_rtf_to_text(compressed: &[u8]) -> Option<String> {
    let raw = decompress_rtf(compressed).ok()?;
    let text = normalize_whitespace(&rtf_to_text(&raw));
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_dict_is_207_bytes() {
        assert_eq!(LZFU_INIT_DICT.len(), 207);
    }
}
