//! Page rasterisation for scanned PDFs, via the `liteparse` crate (PDFium).
//!
//! Everything else about PDF — text, layout, images — is now parsed by this
//! crate ([`super::parse`]). What is left here is the one thing a parser cannot
//! do: *render* a page. `supported-formats.mdx` promises that a PDF with no
//! extractable text returns one image per page, and drawing a page means
//! executing its graphics, not reading it.
//!
//! It is therefore native-only and behind the `pdf-native` feature. On wasm32
//! there is no PDFium, so a text-less PDF reports that it has no text rather
//! than returning page renders.

use liteparse::config::LiteParseConfig;
use liteparse::parser::LiteParse;
use liteparse::types::PdfInput;

/// Render every page of a PDF to PNG, keyed `page_{n}.png`.
pub fn render_pages(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start async runtime: {e}"))?;

    let input = PdfInput::Bytes(bytes.to_vec());
    runtime.block_on(async move {
        let config = LiteParseConfig { ocr_enabled: false, quiet: true, ..Default::default() };
        let shots = LiteParse::new(config)
            .screenshot_input(input, None)
            .await
            .map_err(|e| format!("Failed to render PDF pages: {e}"))?;
        Ok(shots.into_iter().map(|s| (format!("page_{}.png", s.page_num), s.image_bytes)).collect())
    })
}
