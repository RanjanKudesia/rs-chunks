//! Paragraph properties (PAPX) for legacy `.doc`.
//!
//! [MS-DOC] stores paragraph formatting in *formatted disk pages* — 512-byte
//! `PapxFkp` structures in the WordDocument stream, indexed by a `PlcfBtePapx`
//! in the table stream. Each entry names the paragraph's style (`istd`) and a
//! `grpprl` of sprm overrides, which is where table membership, list level and
//! page-break-before live.
//!
//! Entries are keyed by **FC** (a byte offset into the WordDocument stream),
//! not by paragraph ordinal, so resolving them needs the piece table — see
//! [`index_by_paragraph`].

use super::piece_table::{Piece, ReconstructedText};

#[derive(Debug, Clone, Default)]
pub struct ParagraphProp {
    /// FC of the paragraph's first character.
    pub start_fc: u32,
    pub istd: u16,
    /// `sprmPFInTable` — the paragraph is a table cell (or a row mark).
    pub f_in_table: bool,
    /// `sprmPFTtp` — the paragraph is the *table terminating paragraph*, i.e.
    /// the mark that ends a row rather than a cell.
    pub f_tt_p: bool,
    /// `sprmPFPageBreakBefore`.
    pub page_break_before: bool,
    /// `sprmPIlfo` — 1-based index into the list format table; 0 means the
    /// paragraph is not part of a list.
    pub ilfo: u16,
    /// `sprmPIlvl` — 0-based list nesting level, valid only when `ilfo > 0`.
    pub ilvl: u8,
}

const SPRM_P_F_IN_TABLE: u16 = 0x2416;
const SPRM_P_F_TTP: u16 = 0x2417;
const SPRM_P_F_PAGE_BREAK_BEFORE: u16 = 0x2407;
const SPRM_P_ILFO: u16 = 0x460B;
const SPRM_P_ILVL: u16 = 0x260A;

/// Size of a formatted disk page.
const FKP_SIZE: usize = 512;
/// A `BxPap` is a one-byte word offset followed by a 12-byte `PHE`.
const BXPAP_SIZE: usize = 13;

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn parse_grpprl(grpprl: &[u8], prop: &mut ParagraphProp) {
    let mut p = 0usize;
    while p + 2 <= grpprl.len() {
        let opcode = u16::from_le_bytes([grpprl[p], grpprl[p + 1]]);
        p += 2;

        let spra = (opcode >> 13) & 0x7;
        let variable = spra == 6;

        let operand_len = if variable {
            if p >= grpprl.len() {
                break;
            }
            let cb_var = grpprl[p] as usize;
            p += 1;
            cb_var
        } else {
            match spra {
                0 | 1 => 1,
                2 | 4 | 5 => 2,
                3 => 4,
                7 => 3,
                _ => 0,
            }
        };

        let end = match p.checked_add(operand_len) {
            Some(v) => v,
            None => break,
        };
        let op = match grpprl.get(p..end) {
            Some(v) => v,
            None => break,
        };

        match opcode {
            SPRM_P_F_IN_TABLE if !op.is_empty() => {
                prop.f_in_table = (op[0] & 0x01) != 0;
            }
            SPRM_P_F_TTP if !op.is_empty() => {
                prop.f_tt_p = (op[0] & 0x01) != 0;
            }
            SPRM_P_F_PAGE_BREAK_BEFORE if !op.is_empty() => {
                prop.page_break_before = (op[0] & 0x01) != 0;
            }
            SPRM_P_ILFO if op.len() >= 2 => {
                prop.ilfo = u16::from_le_bytes([op[0], op[1]]);
            }
            SPRM_P_ILVL if !op.is_empty() => {
                prop.ilvl = op[0];
            }
            _ => {}
        }

        p = end;
    }
}

/// Decode one `PapxInFkp` at `pos` within an FKP page.
///
/// [MS-DOC] 2.9.175: a leading `cb` of *n* means the record is `2n-1` bytes of
/// `GrpPrlAndIstd`; `cb == 0` means the real size is in the next byte `cb'`,
/// and the record is `2·cb'` bytes. `GrpPrlAndIstd` (2.9.111) is a 2-byte
/// `istd` followed by the sprm list — so the grpprl is 2 bytes shorter than the
/// record either way.
fn parse_papx_in_fkp(page: &[u8], pos: usize) -> Option<(u16, &[u8])> {
    let cb = *page.get(pos)? as usize;
    let (record_start, record_len) = if cb != 0 {
        (pos + 1, (2 * cb).checked_sub(1)?)
    } else {
        let cb2 = *page.get(pos + 1)? as usize;
        (pos + 2, 2 * cb2)
    };
    if record_len < 2 {
        return None;
    }
    let istd = read_u16(page, record_start)?;
    let grpprl_start = record_start + 2;
    let grpprl_end = grpprl_start.checked_add(record_len - 2)?.min(page.len());
    Some((istd, page.get(grpprl_start..grpprl_end).unwrap_or(&[])))
}

/// Walk every `PapxFkp` the `PlcfBtePapx` points at, newest-first order
/// preserved, returning one record per paragraph in the whole document —
/// every story, not just the main text.
pub fn parse_paragraph_props(
    word_doc: &[u8],
    table_stream: &[u8],
    fc_plcf_papx: u32,
    lcb_plcf_papx: u32,
) -> Result<Vec<ParagraphProp>, String> {
    let start = fc_plcf_papx as usize;
    let len = lcb_plcf_papx as usize;
    if len < 8 {
        return Ok(Vec::new());
    }
    let end = start
        .checked_add(len)
        .ok_or_else(|| "PlcfBtePapx offset overflow".to_string())?;
    let plcf = table_stream
        .get(start..end)
        .ok_or_else(|| "PlcfBtePapx points outside table stream".to_string())?;

    // A PLC of n elements holds n+1 CPs/FCs and n data items. Each data item
    // here is a 4-byte FKP page number, so `len = 4(n+1) + 4n = 8n + 4`.
    // Reading it as `len/4 - 1` put the data array's start exactly at the end
    // of the buffer, so *every* lookup fell off the end and this function
    // returned an empty Vec for every `.doc` ever parsed.
    let n = len.saturating_sub(4) / 8;
    if n == 0 {
        return Ok(Vec::new());
    }
    let pn_start = (n + 1) * 4;

    let mut props = Vec::new();

    for i in 0..n {
        let pn = match read_u32(plcf, pn_start + i * 4) {
            Some(v) => v as usize,
            None => continue,
        };
        let page_start = match pn.checked_mul(FKP_SIZE) {
            Some(v) => v,
            None => continue,
        };
        let page = match word_doc.get(page_start..page_start + FKP_SIZE) {
            Some(v) => v,
            None => continue,
        };

        let cpara = page[FKP_SIZE - 1] as usize;
        let rgbx_start = (cpara + 1) * 4;
        if rgbx_start + cpara * BXPAP_SIZE > FKP_SIZE - 1 {
            continue;
        }

        for j in 0..cpara {
            // rgfc[j] is the FC of the paragraph's first character; rgfc[j+1]
            // is one past its paragraph mark.
            let start_fc = match read_u32(page, j * 4) {
                Some(v) => v,
                None => continue,
            };
            let b_offset = page[rgbx_start + j * BXPAP_SIZE] as usize;
            if b_offset == 0 {
                // The paragraph uses its style's properties unchanged.
                continue;
            }
            let Some((istd, grpprl)) = parse_papx_in_fkp(page, b_offset * 2) else {
                continue;
            };

            let mut prop = ParagraphProp {
                start_fc,
                istd,
                ..Default::default()
            };
            parse_grpprl(grpprl, &mut prop);
            props.push(prop);
        }
    }

    props.sort_by_key(|p| p.start_fc);
    Ok(props)
}

/// Map a WordDocument-stream FC to its CP through the piece table.
fn fc_to_cp(pieces: &[Piece], fc: usize) -> Option<u32> {
    for piece in pieces {
        let chars = (piece.cp_end - piece.cp_start) as usize;
        let width = if piece.compressed { 1 } else { 2 };
        let byte_len = chars.checked_mul(width)?;
        if fc >= piece.fc && fc < piece.fc + byte_len {
            return Some(piece.cp_start + ((fc - piece.fc) / width) as u32);
        }
    }
    None
}

/// Resolve the document-wide property list against one story, returning a
/// vector parallel to that story's Word paragraphs.
///
/// The FKPs are keyed by FC and cover every story at once, so an entry only
/// belongs here if its FC maps into *these* pieces. Paragraphs whose entry says
/// "style defaults apply" are absent from the FKP walk and stay `None`.
pub fn index_by_paragraph(
    props: &[ParagraphProp],
    pieces: &[Piece],
    story: &ReconstructedText,
) -> Vec<Option<ParagraphProp>> {
    let mut out = vec![None; story.paragraph_start_cps.len()];
    for prop in props {
        let Some(cp) = fc_to_cp(pieces, prop.start_fc as usize) else {
            continue;
        };
        let Some(idx) = story.paragraph_of_cp(cp) else {
            continue;
        };
        if let Some(slot) = out.get_mut(idx) {
            *slot = Some(prop.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a one-page PAPX FKP holding `entries` of (start_fc, papx bytes),
    /// plus the PlcfBtePapx that points at it.
    fn fkp_document(entries: &[(u32, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let cpara = entries.len();
        let mut page = vec![0u8; FKP_SIZE];
        // The PAPX records live after the rgfc + rgbx arrays, at even offsets.
        let mut write_at = (cpara + 1) * 4 + cpara * BXPAP_SIZE;
        write_at += write_at % 2;

        for (j, (fc, papx)) in entries.iter().enumerate() {
            page[j * 4..j * 4 + 4].copy_from_slice(&fc.to_le_bytes());
            page[(cpara + 1) * 4 + j * BXPAP_SIZE] = (write_at / 2) as u8;
            page[write_at..write_at + papx.len()].copy_from_slice(papx);
            write_at += papx.len();
            write_at += write_at % 2;
        }
        // rgfc needs a final "one past the last paragraph" entry.
        let last = entries.last().map(|(fc, _)| fc + 1).unwrap_or(0);
        page[cpara * 4..cpara * 4 + 4].copy_from_slice(&last.to_le_bytes());
        page[FKP_SIZE - 1] = cpara as u8;

        // WordDocument: page 0 is the FIB (unused here), page 1 is our FKP.
        let mut word_doc = vec![0u8; FKP_SIZE * 2];
        word_doc[FKP_SIZE..].copy_from_slice(&page);

        // PlcfBtePapx for one FKP: 2 FCs + 1 page number = 12 bytes.
        let mut plcf = Vec::new();
        plcf.extend_from_slice(&0u32.to_le_bytes());
        plcf.extend_from_slice(&u32::MAX.to_le_bytes());
        plcf.extend_from_slice(&1u32.to_le_bytes());
        (word_doc, plcf)
    }

    /// `cb`-prefixed PapxInFkp carrying `istd` and a sprm list.
    fn papx(istd: u16, sprms: &[u8]) -> Vec<u8> {
        let mut body = istd.to_le_bytes().to_vec();
        body.extend_from_slice(sprms);
        // Record is 2·cb-1 bytes, so pad to an odd length.
        if body.len().is_multiple_of(2) {
            body.push(0);
        }
        let cb = body.len().div_ceil(2);
        let mut out = vec![cb as u8];
        out.extend_from_slice(&body);
        out
    }

    fn sprm_u8(opcode: u16, value: u8) -> Vec<u8> {
        let mut v = opcode.to_le_bytes().to_vec();
        v.push(value);
        v
    }

    fn sprm_u16(opcode: u16, value: u16) -> Vec<u8> {
        let mut v = opcode.to_le_bytes().to_vec();
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    /// The shipped walk read the PLC's data array at `(len/4)·4`, which is the
    /// end of the buffer — so it produced nothing for every `.doc`, and with it
    /// every table, list and style-based heading went undetected.
    #[test]
    fn a_single_fkp_entry_is_found_at_all() {
        let (word_doc, plcf) = fkp_document(&[(100, papx(7, &[]))]);
        let props = parse_paragraph_props(&word_doc, &plcf, 0, plcf.len() as u32).unwrap();
        assert_eq!(props.len(), 1, "the one PAPX entry must be read");
        assert_eq!(props[0].istd, 7);
        assert_eq!(props[0].start_fc, 100);
    }

    #[test]
    fn sprms_decode_table_list_and_page_break_flags() {
        let mut sprms = sprm_u8(SPRM_P_F_IN_TABLE, 1);
        sprms.extend(sprm_u8(SPRM_P_F_TTP, 1));
        sprms.extend(sprm_u8(SPRM_P_F_PAGE_BREAK_BEFORE, 1));
        sprms.extend(sprm_u16(SPRM_P_ILFO, 3));
        sprms.extend(sprm_u8(SPRM_P_ILVL, 2));

        let (word_doc, plcf) = fkp_document(&[(0, papx(1, &sprms))]);
        let props = parse_paragraph_props(&word_doc, &plcf, 0, plcf.len() as u32).unwrap();

        assert_eq!(props.len(), 1);
        let p = &props[0];
        assert!(p.f_in_table, "sprmPFInTable");
        assert!(p.f_tt_p, "sprmPFTtp");
        assert!(p.page_break_before, "sprmPFPageBreakBefore");
        assert_eq!(p.ilfo, 3, "sprmPIlfo");
        assert_eq!(p.ilvl, 2, "sprmPIlvl — the list nesting level (#12)");
    }

    /// A grpprl only reaches the sprm parser if the record length is right:
    /// `2·cb-1` bytes total, of which the first two are the istd. Reading it as
    /// `cb-1` truncated every sprm list to under half its size.
    #[test]
    fn grpprl_length_covers_every_sprm_in_the_record() {
        // Five sprms — 12 bytes — is longer than `cb-1` would ever allow.
        let mut sprms = sprm_u16(SPRM_P_ILFO, 9);
        sprms.extend(sprm_u8(SPRM_P_ILVL, 4));
        sprms.extend(sprm_u8(SPRM_P_F_IN_TABLE, 0));
        sprms.extend(sprm_u8(SPRM_P_F_TTP, 0));
        sprms.extend(sprm_u8(SPRM_P_F_PAGE_BREAK_BEFORE, 1));

        let (word_doc, plcf) = fkp_document(&[(0, papx(2, &sprms))]);
        let props = parse_paragraph_props(&word_doc, &plcf, 0, plcf.len() as u32).unwrap();
        // page_break_before is the *last* sprm, so it only decodes when the
        // whole grpprl is in range.
        assert!(
            props[0].page_break_before,
            "trailing sprm was truncated away"
        );
        assert_eq!(props[0].ilvl, 4);
    }

    #[test]
    fn cb_zero_records_read_their_length_from_the_next_byte() {
        let sprms = sprm_u8(SPRM_P_ILVL, 5);
        let mut body = 3u16.to_le_bytes().to_vec();
        body.extend_from_slice(&sprms);
        if !body.len().is_multiple_of(2) {
            body.push(0);
        }
        let mut record = vec![0u8, (body.len() / 2) as u8];
        record.extend_from_slice(&body);

        let (word_doc, plcf) = fkp_document(&[(0, record)]);
        let props = parse_paragraph_props(&word_doc, &plcf, 0, plcf.len() as u32).unwrap();
        assert_eq!(props[0].istd, 3);
        assert_eq!(props[0].ilvl, 5);
    }

    #[test]
    fn properties_land_on_the_paragraph_they_describe() {
        // Two compressed pieces are not needed — one 12-character story with a
        // paragraph mark at CP 4 and a cell mark at CP 8.
        let word_doc = b"abc\rdef\xffghi\r".to_vec();
        let mut word_doc = word_doc;
        word_doc[7] = 0x07;
        let pieces = vec![Piece {
            cp_start: 0,
            cp_end: 12,
            fc: 0,
            compressed: true,
        }];
        let story = super::super::piece_table::reconstruct_from_pieces(&word_doc, &pieces);
        // The trailing `\r` opens a fourth, empty paragraph — exactly what
        // splitting the text on its terminators produces.
        assert_eq!(story.paragraph_start_cps, vec![0, 4, 8, 12]);

        let props = vec![
            ParagraphProp {
                start_fc: 0,
                istd: 1,
                ..Default::default()
            },
            ParagraphProp {
                start_fc: 4,
                istd: 2,
                ..Default::default()
            },
            ParagraphProp {
                start_fc: 8,
                istd: 3,
                ..Default::default()
            },
        ];
        let indexed = index_by_paragraph(&props, &pieces, &story);
        let istds: Vec<Option<u16>> = indexed.iter().map(|p| p.as_ref().map(|p| p.istd)).collect();
        assert_eq!(istds, vec![Some(1), Some(2), Some(3), None]);
    }

    /// The FKPs cover every story at once, so an entry whose FC is not in this
    /// story's pieces must not be attributed to one of its paragraphs.
    #[test]
    fn properties_from_another_story_are_not_borrowed() {
        let word_doc = b"abc\rdef\r".to_vec();
        let pieces = vec![Piece {
            cp_start: 0,
            cp_end: 8,
            fc: 0,
            compressed: true,
        }];
        let story = super::super::piece_table::reconstruct_from_pieces(&word_doc, &pieces);
        let props = vec![ParagraphProp {
            start_fc: 9_000,
            istd: 4,
            ..Default::default()
        }];
        let indexed = index_by_paragraph(&props, &pieces, &story);
        assert!(indexed.iter().all(Option::is_none));
    }
}
