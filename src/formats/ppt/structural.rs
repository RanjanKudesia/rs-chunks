use std::path::Path;

use crate::formats::doc::text_extractor::DocParagraph;

use super::cfb_reader;
use super::text_extractor;

pub(crate) fn validate_ppt_path(file_path: &str) -> Result<(), String> {
    if !file_path.to_ascii_lowercase().ends_with(".ppt") {
        return Err(format!("Expected .ppt file path, got: {file_path}"));
    }
    if !Path::new(file_path).exists() {
        return Err(format!("File not found: {file_path}"));
    }
    Ok(())
}

pub(crate) fn load_ppt_paragraphs(
    file_path: &str,
) -> Result<(Vec<DocParagraph>, Option<usize>), String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .ppt file: {e}"))?;
    load_ppt_paragraphs_bytes(&bytes)
}

/// Returns the paragraphs plus, when the persist directory resolved, the LIVE
/// slide count. Deriving the count from paragraphs undercounts whenever a
/// trailing slide carries no text (poi_bullets.ppt: 2 live slides, the second
/// text-free), and the live list is the only place the true denominator exists.
pub(crate) fn load_ppt_paragraphs_bytes(
    bytes: &[u8],
) -> Result<(Vec<DocParagraph>, Option<usize>), String> {
    // One container open for both streams (the X9 rule), and the liveness
    // model resolved before any text is read — see `persist.rs` for why a
    // linear scan of a multi-save deck emits deleted content.
    let mut cfb = cfb_reader::PptCfb::open(bytes)?;
    let (stream, current_user) = cfb.document_and_current_user()?;
    let live = super::persist::resolve(&stream, current_user.as_deref());
    let total = match &live {
        super::persist::LiveModel::Persist { slide_offsets, .. } if !slide_offsets.is_empty() => {
            Some(slide_offsets.len())
        }
        _ => None,
    };
    Ok((text_extractor::extract_paragraphs_live(&stream, &live), total))
}
