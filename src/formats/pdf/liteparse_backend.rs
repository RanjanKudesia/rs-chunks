//! PDF → markdown conversion via the `liteparse` crate (heuristic, model-free).
//!
//! liteparse classifies PDF text into markdown elements (headings, paragraphs,
//! tables, lists) from spatial/font signals — far cleaner than raw pdfium text
//! extraction. It does not chunk; chunking is done by feeding this markdown to
//! the existing Markdown chunker (see `chunkers.rs`).

use liteparse::config::{ImageMode, LiteParseConfig, OutputFormat};
use liteparse::parser::LiteParse;

/// One extracted raster image: the markdown reference key (`image_{id}.png`) and
/// its bytes. Keyed to match the `![](image_{id}.png)` refs liteparse emits.
pub struct PdfImage {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Convert a PDF file to a single markdown string (pages joined by `---`).
/// When `embed_images` is true, image bytes are returned alongside; otherwise
/// the markdown still carries `![](…)` placeholders but no bytes are collected.
/// Markdown + images + total page count from one liteparse pass.
pub struct PdfConversion {
    pub markdown: String,
    pub images: Vec<PdfImage>,
    pub total_pages: usize,
}

pub fn pdf_to_markdown(
    file_path: &str,
    embed_images: bool,
) -> Result<PdfConversion, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start async runtime: {e}"))?;

    let path = file_path.to_string();
    runtime.block_on(async move {
        let mut config = LiteParseConfig::default();
        config.ocr_enabled = false;
        config.output_format = OutputFormat::Markdown;
        config.image_mode = if embed_images {
            ImageMode::Embed
        } else {
            ImageMode::Placeholder
        };
        config.quiet = true;

        let parser = LiteParse::new(config);
        let result = parser
            .parse(&path)
            .await
            .map_err(|e| format!("liteparse failed to parse PDF: {e}"))?;

        let total_pages = result.pages.len();
        let markdown = result
            .pages
            .iter()
            .map(|p| p.markdown.trim_end().to_string())
            .filter(|m| !m.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let images = if embed_images {
            result
                .images
                .into_iter()
                .map(|img| PdfImage {
                    // liteparse's markdown emitter references images as
                    // `![](image_{id}.png)` regardless of the source codec.
                    name: format!("image_{}.png", img.id),
                    bytes: (*img.bytes).clone(),
                })
                .collect()
        } else {
            Vec::new()
        };

        Ok(PdfConversion {
            markdown,
            images,
            total_pages,
        })
    })
}
