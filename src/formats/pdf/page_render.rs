//! Rendering a page to a raster — the one part of PDF this crate does not own.
//!
//! Reading a PDF is parsing; *drawing* one is executing its graphics, which
//! needs a rasteriser. `supported-formats.mdx` promises that a scanned PDF with
//! no extractable text returns one image per page ([#56](TECH_DEBT.md)), and
//! that promise is kept natively by PDFium, behind the default `pdf-native`
//! feature.
//!
//! On wasm32 there is no PDFium, so a text-less PDF reports that it has no
//! text. That is the only remaining behavioural difference between the SDKs for
//! PDF, and it is stated in the docs rather than papered over.

use crate::error::Result;

#[cfg(feature = "pdf-native")]
pub(crate) fn render_pages(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    super::pdfium_render::render_pages(bytes).map_err(crate::error::ChunkError::Parse)
}

#[cfg(not(feature = "pdf-native"))]
pub(crate) fn render_pages(_bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    Ok(Vec::new())
}
