pub struct ReconstructedText {
    pub text: String,
    /// CP→byte-offset map built during piece-table reconstruction; retained for
    /// future character-position mapping (e.g. field/bookmark spans), not yet read.
    #[allow(dead_code)]
    pub cp_to_byte: Vec<usize>,
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("Piece table truncated at offset {offset}"))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("Piece table truncated at offset {offset}"))
}

fn cp1252_to_char(byte: u8) -> char {
    match byte {
        0x00..=0x7F => byte as char,
        0x80 => '\u{20AC}',
        0x81 => '\u{0081}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8D => '\u{008D}',
        0x8E => '\u{017D}',
        0x8F => '\u{008F}',
        0x90 => '\u{0090}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9D => '\u{009D}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        0xA0..=0xFF => char::from_u32(byte as u32).unwrap_or('\u{FFFD}'),
    }
}

fn normalize_doc_char(ch: char) -> Option<char> {
    match ch {
        '\r' => Some('\r'),
        '\x07' => Some('\x07'),
        '\x0C' => Some('\x0C'),
        '\x0B' => Some('\n'),
        // Drop NUL and all non-text C0/DEL control characters. NUL in particular
        // is never document text — some `.doc` files (e.g. large ones padded to
        // size) have piece-table runs pointing at NUL padding, which must not
        // leak into the extracted text as an unsplittable binary blob.
        '\x00'..='\x06' | '\x08'..='\x09' | '\x0E'..='\x1F' | '\x7F' => None,
        other => Some(other),
    }
}

/// One text piece from the Plcpcd: a run of `cp_end - cp_start` characters
/// stored at byte offset `fc` in the WordDocument stream, 8-bit (cp1252)
/// when `compressed`, UTF-16LE otherwise. `cp_end` is already clamped to
/// `ccpText`. Semantics mirror `parse_piece_table` exactly.
#[derive(Debug, Clone)]
pub struct Piece {
    pub cp_start: u32,
    pub cp_end: u32,
    pub fc: usize,
    pub compressed: bool,
}

/// Parse the CLX/Plcpcd into the raw piece list without decoding text.
/// Used by the image extractor to map file offsets (FC) to character
/// positions (CP) and to count paragraph marks before an anchor CP.
pub fn parse_pieces(
    table_stream: &[u8],
    fc_clx: u32,
    lcb_clx: u32,
    ccp_text: i32,
) -> Result<Vec<Piece>, String> {
    parse_pieces_range(table_stream, fc_clx, lcb_clx, 0, ccp_text)
}

/// Pieces covering the half-open CP range `[cp_from, ccp_text)`.
pub fn parse_pieces_range(
    table_stream: &[u8],
    fc_clx: u32,
    lcb_clx: u32,
    cp_from: u32,
    ccp_text: i32,
) -> Result<Vec<Piece>, String> {
    let clx_start = fc_clx as usize;
    let clx_len = lcb_clx as usize;
    let clx_end = clx_start
        .checked_add(clx_len)
        .ok_or_else(|| "CLX overflow".to_string())?;
    let clx = table_stream
        .get(clx_start..clx_end)
        .ok_or_else(|| "CLX points outside table stream".to_string())?;

    let mut cursor = 0usize;
    let mut plcpcd: Option<&[u8]> = None;

    while cursor < clx.len() {
        let tag = clx[cursor];
        cursor += 1;
        match tag {
            0x01 => {
                let cb = read_u16(clx, cursor)? as usize;
                cursor = cursor
                    .checked_add(2 + cb)
                    .ok_or_else(|| "CLX grpprl overflow".to_string())?;
                if cursor > clx.len() {
                    return Err("CLX grpprl exceeds bounds".to_string());
                }
            }
            0x02 => {
                let cb = read_u32(clx, cursor)? as usize;
                cursor += 4;
                let end = cursor
                    .checked_add(cb)
                    .ok_or_else(|| "CLX Plcpcd overflow".to_string())?;
                let data = clx
                    .get(cursor..end)
                    .ok_or_else(|| "CLX Plcpcd exceeds bounds".to_string())?;
                plcpcd = Some(data);
                break;
            }
            _ => break,
        }
    }

    let plcpcd = match plcpcd {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    if plcpcd.len() < 4 || (plcpcd.len() - 4) % 12 != 0 {
        return Err("Invalid Plcpcd size".to_string());
    }

    let n = (plcpcd.len() - 4) / 12;
    if n == 0 {
        return Ok(Vec::new());
    }

    let cp_count = n + 1;
    let cp_bytes = cp_count * 4;
    if cp_bytes > plcpcd.len() {
        return Err("Invalid Plcpcd CP array".to_string());
    }

    let mut cps = Vec::with_capacity(cp_count);
    for i in 0..cp_count {
        cps.push(read_u32(plcpcd, i * 4)?);
    }

    let pcd_start = cp_bytes;
    // `ccp_text` is the exclusive CP limit; `cp_from` the inclusive start. The
    // main text is [0, ccpText), and each later story is its own slice of the
    // same piece table — see [MS-DOC] 2.5.4. (#70)
    let cp_hi = ccp_text.max(0) as u32;
    let cp_lo = cp_from;
    let mut pieces = Vec::with_capacity(n);

    for i in 0..n {
        let piece_start = cps[i];
        let piece_end = cps[i + 1];
        if piece_end <= piece_start || piece_start >= cp_hi || piece_end <= cp_lo {
            continue;
        }

        // Clip the piece to the requested range, advancing `fc` by however many
        // characters we skipped at the front (1 byte each when compressed, 2
        // when UTF-16).
        let start_cp = piece_start.max(cp_lo);
        let end_cp = piece_end.min(cp_hi);
        if end_cp <= start_cp {
            continue;
        }
        let skipped = (start_cp - piece_start) as usize;
        let char_count = (end_cp - start_cp) as usize;

        let pcd_off = pcd_start + i * 8;
        // [MS-DOC] 2.9.74 FcCompressed: bits 0-29 hold fc, bit 30 is fCompressed,
        // bit 31 is r1 (reserved). Two things were wrong here.
        //
        // The mask cleared only bit 30, leaving a set reserved bit in the
        // offset. And for a compressed (single-byte, cp1252) piece, fc is
        // *twice* the real byte offset — the spec stores it in the same units
        // as an uncompressed piece — so it must be halved. Without that, every
        // piece was read from double its true position: on sample.doc the title,
        // author line, whole Abstract and the INTRODUCTION heading vanished, and
        // the text began and ended mid-word.
        let fc_raw = read_u32(plcpcd, pcd_off + 2)?;
        let compressed = (fc_raw & 0x4000_0000) != 0;
        let fc = (fc_raw & 0x3FFF_FFFF) as usize;
        let base = if compressed { fc / 2 } else { fc };
        let real_offset = base + skipped * if compressed { 1 } else { 2 };

        pieces.push(Piece {
            cp_start: start_cp,
            cp_end: start_cp + char_count as u32,
            fc: real_offset,
            compressed,
        });
    }

    Ok(pieces)
}

/// Reconstruct the text of a single story — the CP range `[cp_from, cp_to)`.
///
/// A `.doc`'s CP space runs main text, then footnotes, headers/footers,
/// annotations, endnotes and text boxes, each with its own length in the FIB.
/// Only the main text was ever read, so on sample.doc a footnote block, a
/// header and a text box holding a whole table were silently dropped. (#70)
pub fn reconstruct_story(
    word_doc: &[u8],
    table_stream: &[u8],
    fc_clx: u32,
    lcb_clx: u32,
    cp_from: u32,
    cp_to: u32,
) -> Option<String> {
    if cp_to <= cp_from {
        return None;
    }
    let pieces =
        parse_pieces_range(table_stream, fc_clx, lcb_clx, cp_from, cp_to as i32).ok()?;
    let text = reconstruct_from_pieces(word_doc, &pieces).text;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn parse_piece_table(
    word_doc: &[u8],
    table_stream: &[u8],
    fc_clx: u32,
    lcb_clx: u32,
    ccp_text: i32,
) -> Result<ReconstructedText, String> {
    let pieces = parse_pieces(table_stream, fc_clx, lcb_clx, ccp_text)?;
    Ok(reconstruct_from_pieces(word_doc, &pieces))
}

/// Decode a piece list into text plus the CP→byte map.
pub fn reconstruct_from_pieces(word_doc: &[u8], pieces: &[Piece]) -> ReconstructedText {
    let mut text = String::new();
    let mut cp_to_byte = Vec::new();

    for piece in pieces {
        let char_count = (piece.cp_end - piece.cp_start) as usize;
        let real_offset = piece.fc;

        if piece.compressed {
            // Overflow here means a corrupt offset; skip the piece rather than
            // failing the whole document.
            let Some(end) = real_offset.checked_add(char_count) else {
                continue;
            };
            // A piece that runs past the end of the stream used to be dropped
            // whole. Take what is actually there instead: sample.doc's last
            // piece overran slightly and the text ended mid-word.
            let bytes = match word_doc.get(real_offset..end.min(word_doc.len())) {
                Some(b) => b,
                None => continue,
            };
            for b in bytes {
                if let Some(ch) = normalize_doc_char(cp1252_to_char(*b)) {
                    cp_to_byte.push(text.len());
                    text.push(ch);
                }
            }
        } else {
            let Some(byte_count) = char_count.checked_mul(2) else {
                continue;
            };
            let Some(end) = real_offset.checked_add(byte_count) else {
                continue;
            };
            // Same clamp as the compressed branch — keep the readable prefix
            // rather than discarding the piece.
            let bytes = match word_doc.get(real_offset..end.min(word_doc.len())) {
                Some(b) => b,
                None => continue,
            };

            let mut units = Vec::with_capacity(bytes.len() / 2);
            let mut k = 0usize;
            while k + 1 < bytes.len() {
                units.push(u16::from_le_bytes([bytes[k], bytes[k + 1]]));
                k += 2;
            }

            for decoded in char::decode_utf16(units.into_iter()) {
                let ch = decoded.unwrap_or('\u{FFFD}');
                if let Some(ch) = normalize_doc_char(ch) {
                    cp_to_byte.push(text.len());
                    text.push(ch);
                }
            }
        }
    }

    ReconstructedText { text, cp_to_byte }
}
