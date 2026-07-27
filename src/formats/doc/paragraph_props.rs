#[derive(Debug, Clone)]
pub struct ParagraphProp {
    pub start_cp: u32,
    pub istd: u16,
    pub f_in_table: bool,
    /// Parsed from sprmPFPageBreakBefore; retained for future page-aware
    /// chunking of legacy `.doc`, not yet consumed by the chunker.
    #[allow(dead_code)]
    pub page_break_before: bool,
    pub ilfo: u16,
    /// List nesting level (sprmPIlvl); parsed for structural fidelity, reserved
    /// for future list-depth rendering.
    #[allow(dead_code)]
    pub ilvl: u8,
}

const SPRM_P_F_IN_TABLE: u16 = 0x2416;
const SPRM_P_F_PAGE_BREAK_BEFORE: u16 = 0x2407;
const SPRM_P_ILFO: u16 = 0x460B;
const SPRM_P_ILVL: u16 = 0x260A;

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn parse_grpprl(
    grpprl: &[u8],
    f_in_table: &mut bool,
    page_break_before: &mut bool,
    ilfo: &mut u16,
    ilvl: &mut u8,
) {
    let mut p = 0usize;
    while p + 2 <= grpprl.len() {
        let opcode = u16::from_le_bytes([grpprl[p], grpprl[p + 1]]);
        p += 2;

        let spra = (opcode >> 13) & 0x7;
        let operand_len: usize;
        let variable = spra == 6;

        if variable {
            if p >= grpprl.len() {
                break;
            }
            let cb_var = grpprl[p] as usize;
            p += 1;
            operand_len = cb_var;
        } else {
            operand_len = match spra {
                0 | 1 => 1,
                2 | 4 | 5 => 2,
                3 => 4,
                7 => 3,
                _ => 0,
            };
        }

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
                *f_in_table = (op[0] & 0x01) != 0;
            }
            SPRM_P_F_PAGE_BREAK_BEFORE if !op.is_empty() => {
                *page_break_before = (op[0] & 0x01) != 0;
            }
            SPRM_P_ILFO if op.len() >= 2 => {
                *ilfo = u16::from_le_bytes([op[0], op[1]]);
            }
            SPRM_P_ILVL if !op.is_empty() => {
                *ilvl = op[0];
            }
            _ => {}
        }

        p = end;
    }
}

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

    let n = (len / 4).saturating_sub(1);
    if n == 0 {
        return Ok(Vec::new());
    }

    let mut cp_array = Vec::with_capacity(n + 1);
    for i in 0..=n {
        cp_array.push(read_u32(plcf, i * 4).unwrap_or(0));
    }

    let ipgd_start = (n + 1) * 4;
    let mut props = Vec::new();

    for i in 0..n {
        let ipgd = match read_u32(plcf, ipgd_start + i * 4) {
            Some(v) => v as usize,
            None => continue,
        };

        let page_start = match ipgd.checked_mul(512) {
            Some(v) => v,
            None => continue,
        };
        let page_end = match page_start.checked_add(512) {
            Some(v) => v,
            None => continue,
        };
        let page = match word_doc.get(page_start..page_end) {
            Some(v) => v,
            None => continue,
        };

        let cpara = page[511] as usize;
        let fc_bytes = (cpara + 1) * 4;
        let bxpap_start = fc_bytes;
        let bxpap_bytes = cpara * 13;
        if bxpap_start + bxpap_bytes > 511 {
            continue;
        }

        for j in 0..cpara {
            let fc_start = read_u32(page, j * 4).unwrap_or(cp_array[i]);
            let bx = bxpap_start + j * 13;
            let b_offset = page[bx] as usize;
            let pos = b_offset.saturating_mul(2);
            if pos + 4 > page.len() {
                continue;
            }

            let cb = page[pos] as usize;
            let (istd, grpprl_start, grpprl_len) = if cb == 0 {
                if pos + 4 > page.len() {
                    continue;
                }
                let istd = u16::from_le_bytes([page[pos + 2], page[pos + 3]]);
                (istd, pos + 4, 0usize)
            } else {
                if pos + 3 > page.len() {
                    continue;
                }
                let istd = u16::from_le_bytes([page[pos + 1], page[pos + 2]]);
                (istd, pos + 3, cb.saturating_sub(1))
            };

            let grpprl_end = grpprl_start.saturating_add(grpprl_len).min(page.len());
            let grpprl = if grpprl_start <= grpprl_end {
                &page[grpprl_start..grpprl_end]
            } else {
                &[]
            };

            let mut f_in_table = false;
            let mut page_break_before = false;
            let mut ilfo = 0u16;
            let mut ilvl = 0u8;
            parse_grpprl(
                grpprl,
                &mut f_in_table,
                &mut page_break_before,
                &mut ilfo,
                &mut ilvl,
            );

            props.push(ParagraphProp {
                start_cp: fc_start,
                istd,
                f_in_table,
                page_break_before,
                ilfo,
                ilvl,
            });
        }
    }

    props.sort_by_key(|p| p.start_cp);
    Ok(props)
}
