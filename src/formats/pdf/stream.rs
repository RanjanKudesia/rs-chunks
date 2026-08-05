//! Streaming for PDF.
//!
//! **What streams, and what cannot.** The markdown chunkers are whole-document
//! functions, and for PDF they have to be: pagination splits sentences, so the
//! end of page 3 and the start of page 4 are one paragraph and must be chunked
//! as one. Measured on the corpus — chunking each page separately turns
//! `arxiv_1301.3781_word2vec` into 71 chunks instead of 66, breaking a sentence
//! at every page boundary. A chunk therefore cannot be finalised until the page
//! after it has been read, and heading *levels* are ranked across the whole
//! document, so nothing can be emitted from a prefix alone.
//!
//! What streaming *can* do, and what this does:
//!
//! - **Construction returns immediately.** A worker thread parses and chunks
//!   while the caller is free; on `sample-5000-page.pdf` that is 1.6 s off the
//!   calling thread. This is the background thread + channel the README
//!   described and [#55](TECH_DEBT.md) recorded as missing.
//! - **Chunks arrive through a bounded channel**, so a consumer never receives a
//!   pre-materialised collection of all 71,111 of them and can forward each one
//!   as it lands.
//! - **Output is byte-identical to batch**, because it is the same code — pinned
//!   by `stream_matches_batch_for_every_mode` in `tests/pdf_stream.rs`.
//!
//! wasm32 has no threads, so there the work happens on the first `next()` and
//! the chunks drain from there. That is the split `streaming.mdx` already
//! describes under "Profiles, not runtimes".

use crate::chunk::Chunk;
use crate::error::Result;
use crate::formats::pipeline;

/// How many chunks may sit between the worker and the consumer. Big enough that
/// the worker is never stalled by a slow consumer's per-chunk work, small
/// enough that "streaming" means something for memory.
#[cfg(not(target_arch = "wasm32"))]
const CHANNEL_DEPTH: usize = 64;

pub struct PdfChunkStream {
    backend: Backend,
}

enum Backend {
    #[cfg(not(target_arch = "wasm32"))]
    Threaded(std::sync::mpsc::Receiver<Result<Chunk>>),
    /// The work, deferred until the first `next()` and then drained.
    Deferred(Box<Deferred>),
    Draining(std::vec::IntoIter<Chunk>),
    Failed(Option<crate::error::ChunkError>),
    Done,
}

struct Deferred {
    bytes: Vec<u8>,
    mode: String,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
}

impl Deferred {
    fn run(&self) -> Result<Vec<Chunk>> {
        let loaded = super::load(&self.bytes, false, super::headings_for(&self.mode))?;
        pipeline::chunk(
            &loaded,
            &self.mode,
            self.window_size,
            self.overlap,
            self.sentences_per_chunk,
            self.paragraphs_per_page,
        )
    }
}

impl Iterator for PdfChunkStream {
    type Item = Result<Chunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &mut self.backend {
                #[cfg(not(target_arch = "wasm32"))]
                Backend::Threaded(rx) => return rx.recv().ok(),
                Backend::Draining(chunks) => return chunks.next().map(Ok),
                Backend::Failed(error) => {
                    let error = error.take();
                    self.backend = Backend::Done;
                    return error.map(Err);
                }
                Backend::Done => return None,
                Backend::Deferred(work) => {
                    self.backend = match work.run() {
                        Ok(chunks) => Backend::Draining(chunks.into_iter()),
                        Err(error) => Backend::Failed(Some(error)),
                    };
                }
            }
        }
    }
}

/// Stream a PDF's chunks. Construction does no parsing.
pub fn stream_from_bytes(
    bytes: Vec<u8>,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> PdfChunkStream {
    let work = Deferred {
        bytes,
        mode: mode.to_string(),
        window_size,
        overlap,
        sentences_per_chunk,
        paragraphs_per_page,
    };

    #[cfg(not(target_arch = "wasm32"))]
    let backend = {
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_DEPTH);
        std::thread::spawn(move || match work.run() {
            Ok(chunks) => {
                for chunk in chunks {
                    // A closed receiver means the consumer stopped early — an
                    // ordinary outcome, not a failure.
                    if tx.send(Ok(chunk)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = tx.send(Err(error));
            }
        });
        Backend::Threaded(rx)
    };

    #[cfg(target_arch = "wasm32")]
    let backend = Backend::Deferred(Box::new(work));

    PdfChunkStream { backend }
}
