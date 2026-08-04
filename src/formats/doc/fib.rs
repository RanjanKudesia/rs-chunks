pub struct Fib {
    pub f_which_tbl_stm: u8,
    pub fc_clx: u32,
    pub lcb_clx: u32,
    pub fc_plcf_papx: u32,
    pub lcb_plcf_papx: u32,
    pub fc_stshf: u32,
    pub lcb_stshf: u32,
    pub ccp_text: i32,
    /// PlcfBteChpx (character-run FKP index) in the table stream. Optional:
    /// 0/0 when the FIB does not carry the entry. Used only by the image
    /// extractor to locate inline pictures (sprmCPicLocation).
    pub fc_plcf_bte_chpx: u32,
    pub lcb_plcf_bte_chpx: u32,
    /// OfficeArtContent (drawing group / blip store) in the table stream.
    /// Optional: 0/0 when absent (document has no drawings).
    pub fc_dgg_info: u32,
    pub lcb_dgg_info: u32,
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("FIB truncated at offset {offset}"))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("FIB truncated at offset {offset}"))
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, String> {
    data.get(offset..offset + 4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("FIB truncated at offset {offset}"))
}

fn read_fc_lcb_entry(data: &[u8], start: usize, index: usize) -> Result<(u32, u32), String> {
    let off = start + index * 8;
    let fc = read_u32(data, off)?;
    let lcb = read_u32(data, off + 4)?;
    Ok((fc, lcb))
}

pub fn parse_fib(word_doc: &[u8]) -> Result<Fib, String> {
    if word_doc.len() < 34 {
        return Err("FIB truncated at offset 0".to_string());
    }

    let w_ident = read_u16(word_doc, 0)?;
    if w_ident == 0xA5DC {
        return Err(
            "Pre-Word 97 .doc files are not supported. Convert to .docx first.".to_string(),
        );
    }
    if w_ident != 0xA5EC {
        return Err(format!(
            "Not a Word .doc file (unexpected magic 0x{w_ident:04X})"
        ));
    }

    let flags = read_u16(word_doc, 10)?;
    let f_which_tbl_stm = ((flags >> 9) & 1) as u8;

    let csw = read_u16(word_doc, 32)? as usize;
    let fib_rg_w_start = 34usize;
    let fib_rg_w_end = fib_rg_w_start + (2 * csw);
    if fib_rg_w_end > word_doc.len() {
        return Err(format!("FIB truncated at offset {fib_rg_w_end}"));
    }

    let cslw_off = fib_rg_w_end;
    let cslw = read_u16(word_doc, cslw_off)? as usize;
    let lw_array_start = cslw_off + 2;
    let lw_array_end = lw_array_start + (4 * cslw);
    if lw_array_end > word_doc.len() {
        return Err(format!("FIB truncated at offset {lw_array_end}"));
    }
    if cslw < 4 {
        return Err("FIB FibRgLw does not include ccpText".to_string());
    }
    // [MS-DOC] 2.5.4 FibRgLw97: cbMac(0) reserved1(4) reserved2(8) ccpText(12).
    // This read +8 — reserved2 — which on sample.doc holds 30505 against a real
    // ccpText of 30153. The 352-character overrun ran past the end of the main
    // story and into the footnote story, so extraction ended part-way through a
    // footnote, mid-word.
    let ccp_text = read_i32(word_doc, lw_array_start + 12)?;

    let cfclcb_off = lw_array_end;
    let cfclcb = read_u16(word_doc, cfclcb_off)? as usize;
    let fclcb_start = cfclcb_off + 2;
    let fclcb_end = fclcb_start + (8 * cfclcb);
    if fclcb_end > word_doc.len() {
        return Err(format!("FIB truncated at offset {fclcb_end}"));
    }

    for required in [1usize, 13usize, 33usize] {
        if required >= cfclcb {
            return Err(format!("FIB FibRgFcLcb missing entry index {required}"));
        }
    }

    let (fc_stshf, lcb_stshf) = read_fc_lcb_entry(word_doc, fclcb_start, 1)?;
    let (fc_plcf_papx, lcb_plcf_papx) = read_fc_lcb_entry(word_doc, fclcb_start, 13)?;
    let (fc_clx, lcb_clx) = read_fc_lcb_entry(word_doc, fclcb_start, 33)?;

    // Optional entries: PlcfBteChpx (12) and DggInfo (50). Missing or
    // truncated entries degrade to 0/0 — only image extraction uses them.
    let read_optional = |index: usize| -> (u32, u32) {
        if index < cfclcb {
            read_fc_lcb_entry(word_doc, fclcb_start, index).unwrap_or((0, 0))
        } else {
            (0, 0)
        }
    };
    let (fc_plcf_bte_chpx, lcb_plcf_bte_chpx) = read_optional(12);
    let (fc_dgg_info, lcb_dgg_info) = read_optional(50);

    Ok(Fib {
        f_which_tbl_stm,
        fc_clx,
        lcb_clx,
        fc_plcf_papx,
        lcb_plcf_papx,
        fc_stshf,
        lcb_stshf,
        ccp_text,
        fc_plcf_bte_chpx,
        lcb_plcf_bte_chpx,
        fc_dgg_info,
        lcb_dgg_info,
    })
}
