//! Image extraction for legacy `.ppt` files.
//!
//! Image bytes live in the optional "Pictures" CFB stream as back-to-back
//! OfficeArtBlip records. The blip store (OfficeArtBStoreContainer, inside
//! the DggContainer of the "PowerPoint Document" stream) lists one FBSE per
//! image whose `foDelay` is the record's byte offset in the Pictures stream.
//! Shapes on a slide reference images through the `Pib` shape property — a
//! 1-based index into the blip store.
//!
//! Slide attribution follows the same rule the text extractor uses for
//! freeform text: SlideContainer drawings are matched positionally to the
//! extracted slide list only when the counts agree; otherwise images are
//! emitted without a slide number rather than risking misattribution.


use crate::formats::doc::text_extractor::DocParagraph;
use crate::formats::odraw::{
    blip_hash_name, decode_blip_record_at, decode_fbse_blip, find_record, is_blip_record,
    parse_odraw_header, DecodedBlip, RT_BSTORE_CONTAINER, RT_FBSE,
};

use super::cfb_reader;
use super::records::{parse_header, RT_MAIN_MASTER, RT_NOTES_CONTAINER, RT_SLIDE_CONTAINER,
    REC_VER_CONTAINER};
use super::text_extractor;

/// One image occurrence in the presentation. `slide_idx` is a 0-based index
/// into the slide list returned by `text_extractor::extract_slides`, or
/// `None` when the image could not be attributed to a slide.
#[derive(Debug, Clone)]
pub struct PptImage {
    pub hash_name: String,
    pub bytes: Vec<u8>,
    pub slide_idx: Option<usize>,
}

/// Blip-store entries decoded to images (index = 0-based store position).
fn decode_bstore(doc_stream: &[u8], pictures: Option<&[u8]>) -> Vec<Option<DecodedBlip>> {
    let Some(bstore) = find_record(doc_stream, 0, doc_stream.len(), RT_BSTORE_CONTAINER) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut pos = bstore.body_start;
    while let Some((hdr, next)) = parse_odraw_header(doc_stream, pos, bstore.body_end) {
        if next <= pos {
            break;
        }
        let body = &doc_stream[hdr.body_start..hdr.body_end];
        let decoded = if hdr.rec_type == RT_FBSE {
            decode_fbse_blip(body, pictures)
        } else if is_blip_record(hdr.rec_type) {
            decode_blip_record_at(doc_stream, pos)
        } else {
            None
        };
        entries.push(decoded);
        pos = next;
    }
    entries
}

/// Collect `Pib` blip-store references per SlideContainer, in document order.
fn collect_slide_pibs(data: &[u8], start: usize, end: usize, out: &mut Vec<Vec<u32>>) {
    let mut pos = start;
    while let Some((hdr, next)) = parse_header(data, pos, end) {
        if next <= pos {
            break;
        }
        match hdr.rec_type {
            RT_SLIDE_CONTAINER => {
                let mut pibs = Vec::new();
                crate::formats::odraw::collect_pib_values(
                    data,
                    hdr.body_start,
                    hdr.body_end,
                    &mut pibs,
                );
                out.push(pibs);
            }
            RT_NOTES_CONTAINER | RT_MAIN_MASTER => {}
            _ => {
                if hdr.rec_ver == REC_VER_CONTAINER {
                    collect_slide_pibs(data, hdr.body_start, hdr.body_end, out);
                }
            }
        }
        pos = next;
    }
}

/// Decode every blip record found by walking the Pictures stream
/// sequentially. Fallback when no slide/blip-store association is possible.
fn decode_pictures_stream(pictures: &[u8]) -> Vec<DecodedBlip> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some((hdr, next)) = parse_odraw_header(pictures, pos, pictures.len()) {
        if next <= pos {
            break;
        }
        if let Some(decoded) = decode_blip_record_at(pictures, pos) {
            out.push(decoded);
        } else if hdr.rec_type == RT_FBSE {
            // Some writers store the FBSE header inline in the Pictures
            // stream with the blip record embedded right after it.
            if let Some(decoded) =
                decode_fbse_blip(&pictures[hdr.body_start..hdr.body_end], None)
            {
                out.push(decoded);
            }
        }
        pos = next;
    }
    out
}

/// Extract all slide images from a `.ppt` file. One entry per image
/// occurrence on a slide (master/notes images are excluded); duplicates
/// across slides share the same `hash_name`.
pub fn extract_ppt_images(file_path: &str) -> Result<Vec<PptImage>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .ppt file: {e}"))?;
    extract_ppt_images_bytes(&bytes)
}

pub fn extract_ppt_images_bytes(bytes: &[u8]) -> Result<Vec<PptImage>, String> {
    let doc_stream = cfb_reader::read_powerpoint_document_stream(bytes)?;
    let pictures = cfb_reader::read_pictures_stream(bytes)?;

    let bstore = decode_bstore(&doc_stream, pictures.as_deref());

    let mut slide_pibs: Vec<Vec<u32>> = Vec::new();
    collect_slide_pibs(&doc_stream, 0, doc_stream.len(), &mut slide_pibs);

    // Drawing order maps 1:1 onto the extracted slide list only when the
    // counts agree (the rule the text extractor uses for freeform merge).
    let slide_count = text_extractor::extract_slides(&doc_stream).len();
    let aligned = slide_pibs.len() == slide_count;

    let mut out: Vec<PptImage> = Vec::new();
    for (drawing_idx, pibs) in slide_pibs.iter().enumerate() {
        for pib in pibs {
            let idx = (*pib as usize).wrapping_sub(1);
            if let Some(Some(decoded)) = bstore.get(idx) {
                out.push(PptImage {
                    hash_name: blip_hash_name(&decoded.bytes, decoded.ext),
                    bytes: decoded.bytes.clone(),
                    slide_idx: if aligned { Some(drawing_idx) } else { None },
                });
            }
        }
    }

    // Fallback: no slide association was possible at all, but the file does
    // carry pictures — emit them unanchored instead of dropping them.
    if out.is_empty() {
        if let Some(ref pics) = pictures {
            for decoded in decode_pictures_stream(pics) {
                out.push(PptImage {
                    hash_name: blip_hash_name(&decoded.bytes, decoded.ext),
                    bytes: decoded.bytes,
                    slide_idx: None,
                });
            }
        }
    }

    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::odraw::{FBSE_FO_DELAY_OFFSET, FBSE_HEADER_LEN};

    const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    fn record(ver_inst: u16, rec_type: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&ver_inst.to_le_bytes());
        out.extend_from_slice(&rec_type.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn png_blip_record() -> Vec<u8> {
        let mut body = vec![0u8; 17];
        body.extend_from_slice(&PNG_MAGIC);
        body.extend_from_slice(b"data");
        record(0x6E0 << 4, crate::formats::odraw::RT_BLIP_PNG, &body)
    }

    #[test]
    fn decode_bstore_resolves_delay_stream_blip() {
        let mut pictures = vec![0u8; 4];
        let fo_delay = pictures.len() as u32;
        pictures.extend_from_slice(&png_blip_record());

        let mut fbse = vec![0u8; FBSE_HEADER_LEN];
        fbse[FBSE_FO_DELAY_OFFSET..FBSE_FO_DELAY_OFFSET + 4]
            .copy_from_slice(&fo_delay.to_le_bytes());
        let fbse_rec = record(0, RT_FBSE, &fbse);
        let bstore = record((1 << 4) | 0xF, RT_BSTORE_CONTAINER, &fbse_rec);
        // Wrap in a container so find_record has to descend.
        let dgg = record(0xF, crate::formats::odraw::RT_DGG_CONTAINER, &bstore);

        let entries = decode_bstore(&dgg, Some(&pictures));
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_some());
        assert_eq!(entries[0].as_ref().unwrap().ext, ".png");
    }

    #[test]
    fn collect_slide_pibs_groups_by_slide_container() {
        // Slide container holding an OPT record with a Pib property.
        let mut opt_body = Vec::new();
        opt_body.extend_from_slice(&0x4104u16.to_le_bytes());
        opt_body.extend_from_slice(&1u32.to_le_bytes());
        let opt = record((1 << 4) | 0x3, crate::formats::odraw::RT_OPT, &opt_body);
        let slide = record(0xF, RT_SLIDE_CONTAINER, &opt);

        let mut pibs = Vec::new();
        collect_slide_pibs(&slide, 0, slide.len(), &mut pibs);
        assert_eq!(pibs, vec![vec![1]]);
    }

    #[test]
    fn decode_pictures_stream_walks_records() {
        let mut stream = png_blip_record();
        stream.extend_from_slice(&png_blip_record());
        let decoded = decode_pictures_stream(&stream);
        assert_eq!(decoded.len(), 2);
    }
}

/// Native `(chunks, images)` builder for `.ppt` `_with_images` modes: image
/// chunks first (page_number = slide index + 1), then text chunks, indices
/// renumbered across the combined list. Reuses the `.doc` chunk record type.
pub(super) fn chunk_with_images_impl(
    file_path: &str,
    build: impl FnOnce(Vec<DocParagraph>) -> Vec<crate::formats::doc::structural::ChunkRecord>,
) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    super::structural::validate_ppt_path(file_path)?;
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .ppt file: {e}"))?;
    chunk_with_images_impl_bytes(&bytes, file_path, build)
}

/// No-filesystem variant of [`chunk_with_images_impl`] (wasm/browser). `source`
/// is the filename recorded in each chunk's `source` metadata field.
pub(super) fn chunk_with_images_impl_bytes(
    bytes: &[u8],
    source: &str,
    build: impl FnOnce(Vec<DocParagraph>) -> Vec<crate::formats::doc::structural::ChunkRecord>,
) -> Result<(Vec<crate::chunk::Chunk>, Vec<(String, Vec<u8>)>), String> {
    let file_path = source;
    let paragraphs = super::structural::load_ppt_paragraphs_bytes(bytes)?;
    let images = extract_ppt_images_bytes(bytes).unwrap_or_default();
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
                "page_number": img.slide_idx.map(|s| s + 1),
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
                "page_number": serde_json::Value::Null,
            }),
        ));
    }
    Ok((chunk_list, image_out))
}
