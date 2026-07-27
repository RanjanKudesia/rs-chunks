// Minimal PowerPoint binary (MS-PPT) record parsing.
//
// Every record in the "PowerPoint Document" stream begins with an 8-byte
// header: a u16 (recVer in the low 4 bits, recInstance in the high 12 bits),
// a u16 recType, and a u32 recLen (the byte length of the record body that
// follows the header). Container records have recVer == 0xF and hold child
// records; everything else is an atom carrying data.

/// recVer value that marks a container record.
pub const REC_VER_CONTAINER: u16 = 0xF;

// Record types we care about.
pub const RT_SLIDE_CONTAINER: u16 = 0x03EE; // Slide
pub const RT_NOTES_CONTAINER: u16 = 0x03F0; // Notes (skipped)
pub const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3; // marks one slide inside SlideListWithText
pub const RT_MAIN_MASTER: u16 = 0x03F8; // Slide master (skipped)
pub const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0; // Document's ordered slide-text list
pub const RT_TEXT_HEADER_ATOM: u16 = 0x0F9F; // precedes a text atom; data[0..4] = txType
pub const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0; // UTF-16LE text
pub const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8; // Latin-1 text

/// recInstance value of the SlideListWithText that holds slide (not master /
/// notes) text. master = 1, notes = 2.
pub const SLWT_INSTANCE_SLIDES: u16 = 0;

// txType values from a TextHeaderAtom that denote title text.
pub const TX_TYPE_TITLE: u32 = 0;
pub const TX_TYPE_CENTER_TITLE: u32 = 6;

#[derive(Debug, Clone, Copy)]
pub struct RecordHeader {
    pub rec_ver: u16,
    pub rec_instance: u16,
    pub rec_type: u16,
    /// Byte range of the record body within the parent buffer.
    pub body_start: usize,
    pub body_end: usize,
}

fn u16_le(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

/// Parse the record header at `pos` within `data`, clamping the body to `end`.
/// Returns the header and the offset of the next sibling record.
pub fn parse_header(data: &[u8], pos: usize, end: usize) -> Option<(RecordHeader, usize)> {
    if pos + 8 > end {
        return None;
    }
    let ver_inst = u16_le(data, pos);
    let rec_type = u16_le(data, pos + 2);
    let rec_len = u32_le(data, pos + 4) as usize;

    let body_start = pos + 8;
    let body_end = body_start.saturating_add(rec_len).min(end);
    let next = body_end;

    Some((
        RecordHeader {
            rec_ver: ver_inst & 0x000F,
            rec_instance: ver_inst >> 4,
            rec_type,
            body_start,
            body_end,
        },
        next,
    ))
}

/// Read the 4-byte txType payload of a TextHeaderAtom body.
pub fn read_txtype(body: &[u8]) -> Option<u32> {
    if body.len() >= 4 {
        Some(u32_le(body, 0))
    } else {
        None
    }
}
