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

pub(crate) fn load_ppt_paragraphs(file_path: &str) -> Result<Vec<DocParagraph>, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .ppt file: {e}"))?;
    load_ppt_paragraphs_bytes(&bytes)
}

pub(crate) fn load_ppt_paragraphs_bytes(bytes: &[u8]) -> Result<Vec<DocParagraph>, String> {
    let stream = cfb_reader::read_powerpoint_document_stream(bytes)?;
    Ok(text_extractor::extract_paragraphs(&stream))
}

