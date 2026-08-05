
use super::text_extractor::{DocParagraph, ParagraphType};

/// Render a paragraph list to Markdown. Shared by `.doc` and `.ppt`, both of
/// which produce `Vec<DocParagraph>`.
pub(crate) fn render_paragraphs_markdown(paragraphs: Vec<DocParagraph>) -> String {
    let mut rendered: Vec<String> = Vec::new();

    for p in paragraphs {
        match p.paragraph_type {
            ParagraphType::PageBreak => rendered.push("\n\n---\n\n".to_string()),
            ParagraphType::Heading(level) => {
                let content = p.content.trim();
                if content.is_empty() {
                    continue;
                }
                let line = match level {
                    1 => format!("# {content}"),
                    2 | 3 => format!("## {content}"),
                    _ => format!("### {content}"),
                };
                rendered.push(line);
            }
            ParagraphType::ListItem => {
                let content = p.content.trim();
                if content.is_empty() {
                    continue;
                }
                // Two spaces per level is the Markdown convention the shared
                // list parser reads back, so nesting survives a round trip
                // (TECH_DEBT #12, and #27 for the same rule in the md engine).
                let indent = "  ".repeat(p.list_level.unwrap_or(0) as usize);
                rendered.push(format!("{indent}- {content}"));
            }
            ParagraphType::Table => {
                // The content is already a Markdown pipe table — the extractor
                // assembles it from the cell and row marks (TECH_DEBT #12).
                let content = p.content.trim();
                if content.is_empty() {
                    continue;
                }
                rendered.push(content.to_string());
            }
            ParagraphType::Normal => {
                let content = p.content.trim();
                if content.is_empty() {
                    continue;
                }
                rendered.push(content.to_string());
            }
        }
    }

    let mut out = rendered.join("\n\n").trim().to_string();

    while out.ends_with("\n\n---") {
        out.truncate(out.len() - "\n\n---".len());
        out = out.trim_end().to_string();
    }

    out.trim().to_string()
}


/// `.doc` → Markdown with image markers interleaved by raw paragraph ordinal,
/// plus extracted image bytes.
pub(super) fn to_markdown_with_images(file_path: &str) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    super::structural::validate_doc_path(file_path)?;
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .doc file: {e}"))?;
    to_markdown_with_images_bytes(&bytes)
}

pub(super) fn to_markdown_with_images_bytes(bytes: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let indexed = super::structural::load_doc_paragraphs_indexed_bytes(bytes)?;
    let images = super::images::extract_doc_images_bytes(bytes).unwrap_or_default();

    let mut anchored: Vec<(usize, &str)> = images
        .iter()
        .filter_map(|img| img.raw_para.map(|rp| (rp, img.hash_name.as_str())))
        .collect();
    anchored.sort_by_key(|(rp, _)| *rp);
    let mut anchored_iter = anchored.into_iter().peekable();

    let image_paragraph = |hash_name: &str| {
        DocParagraph::plain(format!("![]({hash_name})"), ParagraphType::Normal, None)
    };

    let mut paragraphs: Vec<DocParagraph> = Vec::with_capacity(indexed.len() + images.len());
    for (raw_idx, para) in indexed {
        while let Some((rp, _)) = anchored_iter.peek() {
            if *rp < raw_idx {
                let (_, hash) = anchored_iter.next().unwrap();
                paragraphs.push(image_paragraph(hash));
            } else {
                break;
            }
        }
        paragraphs.push(para);
    }
    for (_, hash) in anchored_iter {
        paragraphs.push(image_paragraph(hash));
    }
    for img in images.iter().filter(|i| i.raw_para.is_none()) {
        paragraphs.push(image_paragraph(&img.hash_name));
    }

    let mut image_out: Vec<(String, Vec<u8>)> = Vec::new();
    for img in &images {
        if !image_out.iter().any(|(n, _)| n == &img.hash_name) {
            image_out.push((img.hash_name.clone(), img.bytes.clone()));
        }
    }
    Ok((render_paragraphs_markdown(paragraphs), image_out))
}
