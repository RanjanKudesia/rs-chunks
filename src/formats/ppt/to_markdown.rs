use crate::formats::doc::text_extractor::{DocParagraph, ParagraphType};

fn image_paragraph(hash_name: &str) -> DocParagraph {
    DocParagraph::plain(format!("![]({hash_name})"), ParagraphType::Normal, None)
}

/// `.ppt` → Markdown with per-slide image markers, plus extracted image bytes.
pub(super) fn to_markdown_with_images(
    file_path: &str,
) -> Result<crate::chunk::MarkdownWithImages, String> {
    super::structural::validate_ppt_path(file_path)?;
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .ppt file: {e}"))?;
    to_markdown_with_images_bytes(&bytes)
}

pub(super) fn to_markdown_with_images_bytes(
    bytes: &[u8],
) -> Result<crate::chunk::MarkdownWithImages, String> {
    let stream = super::cfb_reader::read_powerpoint_document_stream(bytes)?;
    let slides = super::text_extractor::extract_slides(&stream);
    let images = super::images::extract_ppt_images_bytes(bytes).unwrap_or_default();

    let mut paragraphs: Vec<DocParagraph> = Vec::new();
    for (idx, slide) in slides.into_iter().enumerate() {
        let mut slide_paras = slide;
        for img in images.iter().filter(|i| i.slide_idx == Some(idx)) {
            slide_paras.push(image_paragraph(&img.hash_name));
        }
        if slide_paras.is_empty() {
            continue;
        }
        paragraphs.extend(slide_paras);
        paragraphs.push(DocParagraph::plain(
            String::new(),
            ParagraphType::PageBreak,
            None,
        ));
    }
    for img in images.iter().filter(|i| i.slide_idx.is_none()) {
        paragraphs.push(image_paragraph(&img.hash_name));
    }
    while matches!(
        paragraphs.last().map(|p| &p.paragraph_type),
        Some(ParagraphType::PageBreak)
    ) {
        paragraphs.pop();
    }

    let mut image_out: crate::chunk::ExtractedImages = Vec::new();
    for img in &images {
        if !image_out.iter().any(|(n, _)| n == &img.hash_name) {
            image_out.push((img.hash_name.clone(), img.bytes.clone()));
        }
    }
    Ok((
        crate::formats::doc::to_markdown::render_paragraphs_markdown(paragraphs),
        image_out,
    ))
}
