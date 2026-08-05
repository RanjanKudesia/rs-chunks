// The format modules vendor complete parsers ported from the reference engine.
// Some internal helpers (e.g. image-extraction and streaming aux paths) are
// retained for fidelity and future public surface but are not all wired into the
// current API yet — allow the resulting dead-code rather than delete proven code.
#![allow(dead_code)]

//! # chunks-rs
//!
//! Fast, high-fidelity document chunking for RAG pipelines — a pure-Rust engine
//! covering 36 file formats (Office, OpenDocument, PDF, email, ebooks,
//! notebooks, delimited text, and more).
//!
//! Every format returns the same [`Chunk`] shape (`content`, `content_type`,
//! `metadata`), supports multiple chunking strategies ([`ChunkMode`]), and can
//! stream large documents chunk-by-chunk.
//!
//! ```no_run
//! use chunks_rs::formats::csv;
//! let chunks = csv::chunk("data.csv", "row", 10, 5, 1, true, None, "utf-8", true).unwrap();
//! for c in &chunks {
//!     println!("[{}] {}", c.content_type, c.content);
//! }
//! ```

pub mod chunk;
pub mod dispatch;
pub mod error;
pub mod formats;
pub mod options;

pub(crate) mod entities;
pub(crate) mod image_naming;
pub(crate) mod shared;
pub(crate) mod text_encoding;

pub use chunk::Chunk;
pub use dispatch::{
    get_chunks, get_chunks_from_bytes, get_chunks_with_images_from_bytes, get_markdown,
    get_markdown_from_bytes, get_markdown_with_images_from_bytes,
};
pub use error::{ChunkError, Result};
pub use options::{ChunkMode, ChunkOptions};
