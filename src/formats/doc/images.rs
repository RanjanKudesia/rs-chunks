//! Image extraction for legacy `.doc` (Word 97-2003) files.
//!
//! Two storage mechanisms are covered:
//!
//! 1. **Inline pictures** (the common case): a placeholder character in the
//!    text whose CHPX carries `sprmCPicLocation` (0x6A03) — an offset into
//!    the "Data" stream where a PICF header followed by an OfficeArt inline
//!    shape (containing the BLIP) lives. The CHPX run's FC is mapped back to
//!    a CP through the piece table, which lets the picture be anchored to
//!    the raw paragraph that contains it.
//! 2. **Floating shapes**: the FIB's `fcDggInfo` points at the OfficeArt
//!    drawing-group data in the table stream; its blip store (FBSE entries)
//!    references BLIPs via `foDelay` offsets into the WordDocument stream
//!    (the delay stream defined by [MS-DOC] for OfficeArtContent).
//!
//! Extraction is strictly best-effort: any malformed structure skips that
//! image; failures never propagate into text chunking or markdown output.


use crate::formats::odraw::{
    blip_hash_name, decode_blip, decode_fbse_blip, find_record, is_blip_record,
    parse_odraw_header, DecodedBlip, RT_BSTORE_CONTAINER, RT_FBSE, REC_VER_CONTAINER,
};

use super::cfb_reader;
use super::fib::{self, Fib};
use super::piece_table::{self, Piece};

const SPRM_C_PIC_LOCATION: u16 = 0x6A03;

/// One image occurrence in the document. `raw_para` is the ordinal of the
/// raw (`\r`-separated) paragraph containing the picture placeholder, or
/// `None` for floating images that cannot be anchored.
#[derive(Debug, Clone)]
pub struct DocImage {
    pub hash_name: String,
    pub bytes: Vec<u8>,
    pub raw_para: Option<usize>,
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Scan a CHPX grpprl for `sprmCPicLocation` and return its operand (the
/// picture offset in the Data stream). Operand sizes follow the same spra
/// scheme as `paragraph_props::parse_grpprl`.
fn find_pic_location(grpprl: &[u8]) -> Option<u32> {
    let mut p = 0usize;
    let mut result = None;
    while p + 2 <= grpprl.len() {
        let opcode = u16::from_le_bytes([grpprl[p], grpprl[p + 1]]);
        p += 2;

        let spra = (opcode >> 13) & 0x7;
        let operand_len: usize;
        if spra == 6 {
            if p >= grpprl.len() {
                break;
            }
            operand_len = grpprl[p] as usize;
            p += 1;
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
        if opcode == SPRM_C_PIC_LOCATION {
            if let Some(op) = grpprl.get(p..end) {
                if op.len() >= 4 {
                    result = Some(u32::from_le_bytes([op[0], op[1], op[2], op[3]]));
                }
            }
        }
        p = end;
    }
    result
}

/// Walk every character run in the document's CHPX FKPs, invoking `f` with
/// the run's start FC and its grpprl. Mirrors the PAPX FKP walk in
/// `paragraph_props.rs` but with the (simpler) ChpxFkp page layout.
fn walk_chpx_runs(
    word_doc: &[u8],
    table_stream: &[u8],
    fc_plcf_bte_chpx: u32,
    lcb_plcf_bte_chpx: u32,
    mut f: impl FnMut(u32, &[u8]),
) {
    let start = fc_plcf_bte_chpx as usize;
    let len = lcb_plcf_bte_chpx as usize;
    if len < 8 {
        return;
    }
    let Some(end) = start.checked_add(len) else {
        return;
    };
    let Some(plcf) = table_stream.get(start..end) else {
        return;
    };

    // PlcfBteChpx: (n+1) u32 FCs followed by n u32 FKP page numbers, so
    // len = 8n + 4.
    let n = len.saturating_sub(4) / 8;
    if n == 0 {
        return;
    }
    let pn_start = (n + 1) * 4;

    for i in 0..n {
        let Some(pn) = read_u32(plcf, pn_start + i * 4) else {
            continue;
        };
        let Some(page_start) = (pn as usize).checked_mul(512) else {
            continue;
        };
        let Some(page) = word_doc.get(page_start..page_start + 512) else {
            continue;
        };

        let crun = page[511] as usize;
        // rgfc: (crun+1) u32s, then rgb: crun single-byte word offsets.
        let rgb_start = (crun + 1) * 4;
        if rgb_start + crun > 511 {
            continue;
        }

        for j in 0..crun {
            let fc_start = read_u32(page, j * 4).unwrap_or(0);
            let b_offset = page[rgb_start + j] as usize;
            if b_offset == 0 {
                continue; // run uses only the default character properties
            }
            let pos = b_offset * 2;
            if pos >= page.len() {
                continue;
            }
            let cb = page[pos] as usize;
            let grpprl_start = pos + 1;
            let grpprl_end = grpprl_start.saturating_add(cb).min(page.len());
            if grpprl_start >= grpprl_end {
                continue;
            }
            f(fc_start, &page[grpprl_start..grpprl_end]);
        }
    }
}

/// Decode the BLIP stored behind a PICF structure at `fc_pic` in the Data
/// stream. Returns `None` when the offset does not hold a plausible PICF or
/// the payload is not a supported raster format.
fn decode_picf_blip(data_stream: &[u8], fc_pic: usize) -> Option<DecodedBlip> {
    let lcb = read_u32(data_stream, fc_pic)? as usize;
    let cb_header = read_u16(data_stream, fc_pic + 4)? as usize;
    let mm = read_u16(data_stream, fc_pic + 6)?;

    // Sanity: PICF headers are 0x44 bytes; lcb covers header + payload.
    if cb_header < 0x44 || cb_header > lcb || lcb < 0x50 {
        return None;
    }
    let pic_end = fc_pic.checked_add(lcb)?.min(data_stream.len());
    let mut cursor = fc_pic + cb_header;

    // MM_SHAPEFILE (0x66) inserts a length-prefixed picture name between the
    // header and the OfficeArt data.
    if mm == 0x66 {
        let cch = *data_stream.get(cursor)? as usize;
        cursor = cursor + 1 + cch;
    }

    find_blip_in_records(data_stream, cursor, pic_end)
}

/// Walk OfficeArt records in `data[start..end)` (descending into containers
/// and FBSEs) and decode the first supported BLIP found.
fn find_blip_in_records(data: &[u8], start: usize, end: usize) -> Option<DecodedBlip> {
    let mut pos = start;
    while let Some((hdr, next)) = parse_odraw_header(data, pos, end) {
        if next <= pos {
            break;
        }
        if is_blip_record(hdr.rec_type) {
            if let Some(decoded) = decode_blip(
                hdr.rec_type,
                hdr.rec_instance,
                data.get(hdr.body_start..hdr.body_end)?,
            ) {
                return Some(decoded);
            }
        } else if hdr.rec_type == RT_FBSE {
            if let Some(decoded) =
                decode_fbse_blip(data.get(hdr.body_start..hdr.body_end)?, None)
            {
                return Some(decoded);
            }
        } else if hdr.rec_ver == REC_VER_CONTAINER {
            if let Some(decoded) = find_blip_in_records(data, hdr.body_start, hdr.body_end) {
                return Some(decoded);
            }
        }
        pos = next;
    }
    None
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

/// Count paragraph marks (`\r`, 0x0D) that occur before `target_cp` — this
/// equals the raw paragraph ordinal used by `extract_paragraphs_indexed`.
fn count_paragraphs_before(word_doc: &[u8], pieces: &[Piece], target_cp: u32) -> usize {
    let mut count = 0usize;
    for piece in pieces {
        if piece.cp_start >= target_cp {
            break;
        }
        let upto = target_cp.min(piece.cp_end);
        let nchars = (upto - piece.cp_start) as usize;
        if piece.compressed {
            if let Some(bytes) = word_doc.get(piece.fc..piece.fc + nchars) {
                count += bytes.iter().filter(|&&b| b == 0x0D).count();
            }
        } else if let Some(bytes) = word_doc.get(piece.fc..piece.fc + nchars * 2) {
            count += bytes
                .chunks_exact(2)
                .filter(|u| u16::from_le_bytes([u[0], u[1]]) == 0x000D)
                .count();
        }
    }
    count
}

/// Extract all images from a `.doc` file: inline pictures (anchored to their
/// raw paragraph) first, then floating blip-store images not already seen.
pub fn extract_doc_images(file_path: &str) -> Result<Vec<DocImage>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    extract_doc_images_bytes(&bytes)
}

pub fn extract_doc_images_bytes(bytes: &[u8]) -> Result<Vec<DocImage>, String> {
    let word_doc = cfb_reader::read_word_document_stream(bytes)?;
    let fib: Fib = fib::parse_fib(&word_doc)?;
    let table = cfb_reader::read_table_stream(bytes, fib.f_which_tbl_stm)?;
    let data_stream = cfb_reader::read_data_stream(bytes).unwrap_or(None);
    let pieces =
        piece_table::parse_pieces(&table, fib.fc_clx, fib.lcb_clx, fib.ccp_text).unwrap_or_default();

    let mut out: Vec<DocImage> = Vec::new();

    // ── Inline pictures via CHPX / sprmCPicLocation ─────────────────────────
    if let Some(ref data) = data_stream {
        let mut found: Vec<(Option<usize>, DecodedBlip)> = Vec::new();
        let mut seen_fc_pic: Vec<u32> = Vec::new();
        walk_chpx_runs(
            &word_doc,
            &table,
            fib.fc_plcf_bte_chpx,
            fib.lcb_plcf_bte_chpx,
            |fc_start, grpprl| {
                let Some(fc_pic) = find_pic_location(grpprl) else {
                    return;
                };
                // The same CHPX (and thus fcPic) can back several runs.
                if seen_fc_pic.contains(&fc_pic) {
                    return;
                }
                seen_fc_pic.push(fc_pic);
                let Some(decoded) = decode_picf_blip(data, fc_pic as usize) else {
                    return;
                };
                let anchor = fc_to_cp(&pieces, fc_start as usize)
                    .map(|cp| count_paragraphs_before(&word_doc, &pieces, cp));
                found.push((anchor, decoded));
            },
        );
        // Document order = anchor order; unanchored inline images sort last.
        found.sort_by_key(|(anchor, _)| anchor.unwrap_or(usize::MAX));
        for (anchor, decoded) in found {
            out.push(DocImage {
                hash_name: blip_hash_name(&decoded.bytes, decoded.ext),
                bytes: decoded.bytes,
                raw_para: anchor,
            });
        }
    }

    // ── Floating shapes via the drawing-group blip store ────────────────────
    if fib.lcb_dgg_info > 0 {
        let start = fib.fc_dgg_info as usize;
        let end = start.saturating_add(fib.lcb_dgg_info as usize).min(table.len());
        if start < end {
            if let Some(bstore) = find_record(&table, start, end, RT_BSTORE_CONTAINER) {
                let mut pos = bstore.body_start;
                while let Some((hdr, next)) = parse_odraw_header(&table, pos, bstore.body_end) {
                    if next <= pos {
                        break;
                    }
                    let decoded = if hdr.rec_type == RT_FBSE {
                        // The delay stream for Word OfficeArtContent is the
                        // WordDocument stream ([MS-DOC] 2.9.172).
                        decode_fbse_blip(&table[hdr.body_start..hdr.body_end], Some(&word_doc))
                    } else if is_blip_record(hdr.rec_type) {
                        decode_blip(
                            hdr.rec_type,
                            hdr.rec_instance,
                            &table[hdr.body_start..hdr.body_end],
                        )
                    } else {
                        None
                    };
                    if let Some(decoded) = decoded {
                        let hash_name = blip_hash_name(&decoded.bytes, decoded.ext);
                        if !out.iter().any(|img| img.hash_name == hash_name) {
                            out.push(DocImage {
                                hash_name,
                                bytes: decoded.bytes,
                                raw_para: None,
                            });
                        }
                    }
                    pos = next;
                }
            }
        }
    }

    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn find_pic_location_extracts_operand() {
        // sprmCFSpec (0x0855, 1-byte operand) then sprmCPicLocation.
        let mut grpprl = Vec::new();
        grpprl.extend_from_slice(&0x0855u16.to_le_bytes());
        grpprl.push(1);
        grpprl.extend_from_slice(&SPRM_C_PIC_LOCATION.to_le_bytes());
        grpprl.extend_from_slice(&0x1234u32.to_le_bytes());
        assert_eq!(find_pic_location(&grpprl), Some(0x1234));
    }

    #[test]
    fn find_pic_location_none_without_sprm() {
        let mut grpprl = Vec::new();
        grpprl.extend_from_slice(&0x0855u16.to_le_bytes());
        grpprl.push(1);
        assert_eq!(find_pic_location(&grpprl), None);
    }

    #[test]
    fn decode_picf_blip_reads_inline_png() {
        // PICF header (0x44 bytes) + PNG blip record, with lcb covering all.
        let mut blip_body = vec![0u8; 17];
        blip_body.extend_from_slice(&PNG_MAGIC);
        blip_body.extend_from_slice(b"imagedata");
        let mut blip = Vec::new();
        blip.extend_from_slice(&(0x6E0u16 << 4).to_le_bytes());
        blip.extend_from_slice(&crate::formats::odraw::RT_BLIP_PNG.to_le_bytes());
        blip.extend_from_slice(&(blip_body.len() as u32).to_le_bytes());
        blip.extend_from_slice(&blip_body);

        let lcb = (0x44 + blip.len()) as u32;
        let mut data = Vec::new();
        data.extend_from_slice(&lcb.to_le_bytes());
        data.extend_from_slice(&0x44u16.to_le_bytes()); // cbHeader
        data.extend_from_slice(&0x64u16.to_le_bytes()); // mm = MM_SHAPE
        data.resize(0x44, 0);
        data.extend_from_slice(&blip);

        let decoded = decode_picf_blip(&data, 0).expect("inline blip");
        assert_eq!(decoded.ext, ".png");
    }

    #[test]
    fn decode_picf_blip_rejects_garbage() {
        assert!(decode_picf_blip(&[0u8; 16], 0).is_none());
        let mut data = vec![0u8; 256];
        data[0] = 0x10; // lcb tiny
        assert!(decode_picf_blip(&data, 0).is_none());
    }

    #[test]
    fn fc_to_cp_maps_compressed_and_unicode() {
        let pieces = vec![
            Piece {
                cp_start: 0,
                cp_end: 10,
                fc: 100,
                compressed: true,
            },
            Piece {
                cp_start: 10,
                cp_end: 20,
                fc: 200,
                compressed: false,
            },
        ];
        assert_eq!(fc_to_cp(&pieces, 105), Some(5));
        assert_eq!(fc_to_cp(&pieces, 204), Some(12));
        assert_eq!(fc_to_cp(&pieces, 50), None);
    }

    #[test]
    fn count_paragraphs_before_counts_cr() {
        // Compressed piece "ab\rcd\ref" at fc 0.
        let word_doc = b"ab\rcd\ref".to_vec();
        let pieces = vec![Piece {
            cp_start: 0,
            cp_end: 8,
            fc: 0,
            compressed: true,
        }];
        assert_eq!(count_paragraphs_before(&word_doc, &pieces, 0), 0);
        assert_eq!(count_paragraphs_before(&word_doc, &pieces, 4), 1);
        assert_eq!(count_paragraphs_before(&word_doc, &pieces, 8), 2);
    }
}


/// Native `(chunks, images)` builder shared by every `.doc` `_with_images` mode:
/// image chunks first, then text chunks, with chunk indices renumbered across
/// the combined list. Image extraction failures degrade to no images.
pub(super) fn chunk_with_images_impl(
    file_path: &str,
    build: impl FnOnce(Vec<super::text_extractor::DocParagraph>) -> Vec<super::structural::ChunkRecord>,
) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    super::structural::validate_doc_path(file_path)?;
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    chunk_with_images_impl_bytes(&bytes, file_path, build)
}

/// No-filesystem variant of [`chunk_with_images_impl`] (wasm/browser). `source`
/// is the filename recorded in each chunk's `source` metadata field.
pub(super) fn chunk_with_images_impl_bytes(
    bytes: &[u8],
    source: &str,
    build: impl FnOnce(Vec<super::text_extractor::DocParagraph>) -> Vec<super::structural::ChunkRecord>,
) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    let file_path = source;
    let paragraphs = super::structural::load_doc_paragraphs_bytes(bytes)?;
    let images = extract_doc_images_bytes(bytes).unwrap_or_default();
    let text_chunks = build(paragraphs);

    let total = images.len() + text_chunks.len();
    let mut chunk_list: Vec<crate::chunk::Chunk> = Vec::with_capacity(total);
    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();

    for (i, img) in images.iter().enumerate() {
        if !image_out.iter().any(|(n, _)| n == &img.hash_name) {
            image_out.push((img.hash_name.clone(), img.bytes.clone()));
        }
        chunk_list.push(crate::chunk::Chunk::new(
            img.hash_name.clone(),
            "image",
            serde_json::json!({
                "source": file_path,
                "chunk_index": i,
                "total_chunks": total,
                "paragraph_type": "image",
                "heading_level": serde_json::Value::Null,
                "page_number": serde_json::Value::Null,
                "paragraph_index": img.raw_para,
                "image_name": img.hash_name,
            }),
        ));
    }

    let offset = images.len();
    for chunk in &text_chunks {
        chunk_list.push(crate::chunk::Chunk::new(
            chunk.content.clone(),
            chunk.content_type,
            serde_json::json!({
                "source": file_path,
                "chunk_index": chunk.chunk_index + offset,
                "total_chunks": total,
                "paragraph_type": chunk.paragraph_type,
                "heading_level": chunk.heading_level,
                "page_number": chunk.page_number,
            }),
        ));
    }
    Ok((chunk_list, image_out))
}
