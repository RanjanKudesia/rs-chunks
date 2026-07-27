//! Per-format chunking engines. Each format exposes native `chunk` /
//! `chunk_with_options` / `stream` / `to_markdown` entry points.

pub(crate) mod odraw;
pub(crate) mod pipeline;

pub mod csv;
pub mod doc;
pub mod docx;
pub mod eml;
pub mod epub;
pub mod html;
pub mod ipynb;
pub mod json;
pub mod md;
pub mod msg;
pub mod odf;
pub mod pdf;
pub mod ppt;
pub mod pptx;
pub mod rtf;
pub mod txt;
pub mod xlsx;
