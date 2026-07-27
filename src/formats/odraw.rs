//! Shared OfficeArt (MS-ODRAW) record and BLIP parsing.
//!
//! Legacy binary Office formats (.doc, .ppt) store raster images as
//! OfficeArtBlip records. Every OfficeArt record starts with the same 8-byte
//! header used by the MS-PPT record tree: a u16 packing recVer (low 4 bits)
//! and recInstance (high 12 bits), a u16 recType and a u32 recLen. This
//! module provides a header walker plus a BLIP payload decoder shared by the
//! `.doc` and `.ppt` image extractors.

use std::hash::{DefaultHasher, Hash, Hasher};

/// recVer value that marks a container record.
pub const REC_VER_CONTAINER: u16 = 0xF;

// OfficeArt record types used by the image extractors.
// Part of the ODRAW record-type registry; currently referenced only from tests.
#[allow(dead_code)]
pub const RT_DGG_CONTAINER: u16 = 0xF000; // OfficeArtDggContainer (drawing group)
pub const RT_BSTORE_CONTAINER: u16 = 0xF001; // OfficeArtBStoreContainer (blip store)
pub const RT_FBSE: u16 = 0xF007; // OfficeArtFBSE (file blip store entry)
pub const RT_OPT: u16 = 0xF00B; // OfficeArtFOPT (shape properties)
pub const RT_TERTIARY_OPT: u16 = 0xF122; // OfficeArtTertiaryFOPT

// BLIP record types ([MS-ODRAW] 2.2.23 OfficeArtBlip).
pub const RT_BLIP_EMF: u16 = 0xF01A;
pub const RT_BLIP_WMF: u16 = 0xF01B;
pub const RT_BLIP_PICT: u16 = 0xF01C;
pub const RT_BLIP_JPEG: u16 = 0xF01D;
pub const RT_BLIP_PNG: u16 = 0xF01E;
pub const RT_BLIP_DIB: u16 = 0xF01F;
pub const RT_BLIP_TIFF: u16 = 0xF029;
pub const RT_BLIP_JPEG_CMYK: u16 = 0xF02A;

/// Shape property id (low 14 bits of the property opid) that references a
/// blip in the blip store: `Pib` ([MS-ODRAW] 2.3.23.1, property 0x0104).
pub const PROP_PIB: u16 = 0x0104;

/// Minimum size of the fixed OfficeArtFBSE header that precedes any embedded
/// blip record ([MS-ODRAW] 2.2.32).
pub const FBSE_HEADER_LEN: usize = 36;
/// Byte offset of `foDelay` (offset into the host's delay stream) inside the
/// OfficeArtFBSE body.
pub const FBSE_FO_DELAY_OFFSET: usize = 28;

#[derive(Debug, Clone, Copy)]
pub struct OdrawHeader {
    pub rec_ver: u16,
    pub rec_instance: u16,
    pub rec_type: u16,
    /// Byte range of the record body within the parent buffer.
    pub body_start: usize,
    pub body_end: usize,
}

fn u16_le(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_le(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parse the OfficeArt record header at `pos`, clamping the body to `end`.
/// Returns the header and the offset of the next sibling record.
pub fn parse_odraw_header(data: &[u8], pos: usize, end: usize) -> Option<(OdrawHeader, usize)> {
    if pos + 8 > end || end > data.len() {
        return None;
    }
    let ver_inst = u16_le(data, pos)?;
    let rec_type = u16_le(data, pos + 2)?;
    let rec_len = u32_le(data, pos + 4)? as usize;

    let body_start = pos + 8;
    let body_end = body_start.saturating_add(rec_len).min(end);
    Some((
        OdrawHeader {
            rec_ver: ver_inst & 0x000F,
            rec_instance: ver_inst >> 4,
            rec_type,
            body_start,
            body_end,
        },
        body_end,
    ))
}

/// True when `rec_type` is one of the raster/metafile blip record types.
pub fn is_blip_record(rec_type: u16) -> bool {
    matches!(
        rec_type,
        RT_BLIP_EMF
            | RT_BLIP_WMF
            | RT_BLIP_PICT
            | RT_BLIP_JPEG
            | RT_BLIP_PNG
            | RT_BLIP_DIB
            | RT_BLIP_TIFF
            | RT_BLIP_JPEG_CMYK
    )
}

/// A blip payload decoded to a real image byte stream.
#[derive(Debug, Clone)]
pub struct DecodedBlip {
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

fn looks_like_jpeg(bytes: &[u8]) -> bool {
    bytes.len() > 2 && bytes[0] == 0xFF && bytes[1] == 0xD8
}

fn looks_like_png(bytes: &[u8]) -> bool {
    bytes.len() > 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
}

/// Decode a bitmap blip body (the bytes after the 8-byte record header) into
/// raw image bytes. Only JPEG and PNG blips are supported — the same raster
/// formats the DOCX/PPTX extractors accept; EMF/WMF/PICT/TIFF/DIB are skipped
/// (consistent with the `image_hash_name` policy used elsewhere).
///
/// Bitmap blip layout: 16-byte rgbUid1, optional 16-byte rgbUid2 (presence
/// signalled by recInstance), 1-byte tag, then the raw image data. Rather
/// than trusting recInstance blindly, both possible offsets are validated
/// against the image magic bytes.
pub fn decode_blip(rec_type: u16, rec_instance: u16, body: &[u8]) -> Option<DecodedBlip> {
    let (ext, double_uid_instance, check): (&'static str, u16, fn(&[u8]) -> bool) = match rec_type {
        RT_BLIP_JPEG | RT_BLIP_JPEG_CMYK => (".jpg", 0x46B, looks_like_jpeg),
        RT_BLIP_PNG => (".png", 0x6E1, looks_like_png),
        _ => return None,
    };
    // JPEG has two "double uid" instances (RGB 0x46B, CMYK 0x6E3).
    let has_double_uid = rec_instance == double_uid_instance
        || (matches!(rec_type, RT_BLIP_JPEG | RT_BLIP_JPEG_CMYK) && rec_instance == 0x6E3);

    let preferred = if has_double_uid { 33 } else { 17 };
    let fallback = if has_double_uid { 17 } else { 33 };
    for offset in [preferred, fallback] {
        if let Some(data) = body.get(offset..) {
            if check(data) {
                return Some(DecodedBlip {
                    ext,
                    bytes: data.to_vec(),
                });
            }
        }
    }
    None
}

/// Parse the blip record starting at `pos` in `data` (header + payload) and
/// decode it. Returns `None` for non-blip records or unsupported formats.
pub fn decode_blip_record_at(data: &[u8], pos: usize) -> Option<DecodedBlip> {
    let (hdr, _) = parse_odraw_header(data, pos, data.len())?;
    if !is_blip_record(hdr.rec_type) {
        return None;
    }
    decode_blip(
        hdr.rec_type,
        hdr.rec_instance,
        data.get(hdr.body_start..hdr.body_end)?,
    )
}

/// Decode the blip stored in an OfficeArtFBSE body: either embedded directly
/// after the 36-byte fixed header (plus optional name), or reachable through
/// `foDelay` in `delay_stream`.
pub fn decode_fbse_blip(fbse_body: &[u8], delay_stream: Option<&[u8]>) -> Option<DecodedBlip> {
    // Embedded blip: cbName (at offset 33) bytes of name follow the fixed
    // header, then the blip record.
    if fbse_body.len() > FBSE_HEADER_LEN {
        let cb_name = fbse_body.get(33).copied().unwrap_or(0) as usize;
        let blip_start = FBSE_HEADER_LEN + cb_name;
        if blip_start < fbse_body.len() {
            if let Some(decoded) = decode_blip_record_at(fbse_body, blip_start) {
                return Some(decoded);
            }
        }
    }
    // Delay-stream blip: foDelay is the offset of the blip record header.
    let delay = delay_stream?;
    let fo_delay = u32_le(fbse_body, FBSE_FO_DELAY_OFFSET)? as usize;
    if fo_delay == 0xFFFF_FFFF || fo_delay >= delay.len() {
        return None;
    }
    decode_blip_record_at(delay, fo_delay)
}

/// Recursively locate the first record of `wanted_type` within `data[start..end]`.
pub fn find_record(data: &[u8], start: usize, end: usize, wanted_type: u16) -> Option<OdrawHeader> {
    let mut pos = start;
    while let Some((hdr, next)) = parse_odraw_header(data, pos, end) {
        if next <= pos {
            break;
        }
        if hdr.rec_type == wanted_type {
            return Some(hdr);
        }
        if hdr.rec_ver == REC_VER_CONTAINER {
            if let Some(found) = find_record(data, hdr.body_start, hdr.body_end, wanted_type) {
                return Some(found);
            }
        }
        pos = next;
    }
    None
}

/// Collect the values of every `Pib` property (opid low 14 bits == 0x0104) in
/// all OPT records within `data[start..end]`, in document order. The property
/// value is a 1-based index into the blip store.
pub fn collect_pib_values(data: &[u8], start: usize, end: usize, out: &mut Vec<u32>) {
    let mut pos = start;
    while let Some((hdr, next)) = parse_odraw_header(data, pos, end) {
        if next <= pos {
            break;
        }
        if hdr.rec_type == RT_OPT || hdr.rec_type == RT_TERTIARY_OPT {
            // recInstance is the number of fixed 6-byte property entries;
            // complex payloads follow the array and are irrelevant for Pib.
            let count = hdr.rec_instance as usize;
            let body = &data[hdr.body_start..hdr.body_end];
            for i in 0..count {
                let off = i * 6;
                let (Some(opid), Some(value)) = (u16_le(body, off), u32_le(body, off + 2)) else {
                    break;
                };
                if opid & 0x3FFF == PROP_PIB {
                    out.push(value);
                }
            }
        } else if hdr.rec_ver == REC_VER_CONTAINER {
            collect_pib_values(data, hdr.body_start, hdr.body_end, out);
        }
        pos = next;
    }
}

/// Hash image bytes into the library-wide `"<16hex>.<ext>"` naming scheme.
pub fn blip_hash_name(bytes: &[u8], ext: &str) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}{ext}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    fn record(ver_inst: u16, rec_type: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&ver_inst.to_le_bytes());
        out.extend_from_slice(&rec_type.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn png_blip_body(double_uid: bool) -> Vec<u8> {
        let uid_len = if double_uid { 32 } else { 16 };
        let mut body = vec![0u8; uid_len + 1]; // uid(s) + tag
        body.extend_from_slice(&PNG_MAGIC);
        body.extend_from_slice(b"payload");
        body
    }

    #[test]
    fn decode_png_blip_single_uid() {
        let body = png_blip_body(false);
        let decoded = decode_blip(RT_BLIP_PNG, 0x6E0, &body).expect("png should decode");
        assert_eq!(decoded.ext, ".png");
        assert!(looks_like_png(&decoded.bytes));
    }

    #[test]
    fn decode_png_blip_double_uid() {
        let body = png_blip_body(true);
        let decoded = decode_blip(RT_BLIP_PNG, 0x6E1, &body).expect("png should decode");
        assert!(looks_like_png(&decoded.bytes));
    }

    #[test]
    fn decode_blip_wrong_instance_falls_back_to_magic_scan() {
        // Instance says single uid but the payload actually has a double uid.
        let body = png_blip_body(true);
        let decoded = decode_blip(RT_BLIP_PNG, 0x6E0, &body).expect("magic fallback");
        assert!(looks_like_png(&decoded.bytes));
    }

    #[test]
    fn decode_jpeg_blip() {
        let mut body = vec![0u8; 17];
        body.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]);
        let decoded = decode_blip(RT_BLIP_JPEG, 0x46A, &body).expect("jpeg should decode");
        assert_eq!(decoded.ext, ".jpg");
        assert_eq!(decoded.bytes[0], 0xFF);
    }

    #[test]
    fn unsupported_blip_types_return_none() {
        let body = vec![0u8; 64];
        assert!(decode_blip(RT_BLIP_EMF, 0x3D4, &body).is_none());
        assert!(decode_blip(RT_BLIP_WMF, 0x216, &body).is_none());
        assert!(decode_blip(RT_BLIP_DIB, 0x7A8, &body).is_none());
    }

    #[test]
    fn decode_blip_record_at_walks_header() {
        let rec = record(0x6E0 << 4, RT_BLIP_PNG, &png_blip_body(false));
        let decoded = decode_blip_record_at(&rec, 0).expect("record should decode");
        assert_eq!(decoded.ext, ".png");
    }

    #[test]
    fn decode_fbse_embedded_blip() {
        let blip = record(0x6E0 << 4, RT_BLIP_PNG, &png_blip_body(false));
        let mut fbse = vec![0u8; FBSE_HEADER_LEN];
        fbse.extend_from_slice(&blip);
        let decoded = decode_fbse_blip(&fbse, None).expect("embedded blip");
        assert_eq!(decoded.ext, ".png");
    }

    #[test]
    fn decode_fbse_delay_stream_blip() {
        let blip = record(0x6E0 << 4, RT_BLIP_PNG, &png_blip_body(false));
        let mut delay = vec![0u8; 10]; // padding so foDelay != 0
        let fo_delay = delay.len() as u32;
        delay.extend_from_slice(&blip);

        let mut fbse = vec![0u8; FBSE_HEADER_LEN];
        fbse[FBSE_FO_DELAY_OFFSET..FBSE_FO_DELAY_OFFSET + 4]
            .copy_from_slice(&fo_delay.to_le_bytes());
        let decoded = decode_fbse_blip(&fbse, Some(&delay)).expect("delay blip");
        assert_eq!(decoded.ext, ".png");
    }

    #[test]
    fn collect_pib_finds_property() {
        // OPT record with 2 properties: one irrelevant, one Pib (0x4104).
        let mut body = Vec::new();
        body.extend_from_slice(&0x0181u16.to_le_bytes()); // fillColor
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0x4104u16.to_le_bytes()); // Pib (fBid set)
        body.extend_from_slice(&3u32.to_le_bytes());
        let rec = record((2 << 4) | 0x3, RT_OPT, &body); // instance=2, ver=3

        let mut pibs = Vec::new();
        collect_pib_values(&rec, 0, rec.len(), &mut pibs);
        assert_eq!(pibs, vec![3]);
    }

    #[test]
    fn find_record_descends_containers() {
        let inner = record(0, RT_FBSE, &[0u8; 4]);
        let container = record((0 << 4) | REC_VER_CONTAINER, RT_BSTORE_CONTAINER, &inner);
        let found = find_record(&container, 0, container.len(), RT_FBSE);
        assert!(found.is_some());
    }

    #[test]
    fn blip_hash_name_matches_scheme() {
        let name = blip_hash_name(b"abc", ".png");
        assert!(name.ends_with(".png"));
        assert_eq!(name.len(), 16 + 4);
    }
}
