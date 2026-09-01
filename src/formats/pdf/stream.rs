//! Streaming for PDF.
//!
//! **`default` streams for real; the other six modes cannot.** That split is
//! the whole of [#87](TECH_DEBT.md) as it applies to PDF, and it is a property
//! of the *reader*, not of the chunker:
//!
//! - `default` ranks heading sizes within each page (`parse::Headings::PerPage`),
//!   so a page is parsed, rendered and dropped. With a resumable builder
//!   (`StructuralBuilder`) a chunk costs the pages it came from: the first
//!   chunk of `sample-5000-page.pdf` reads fewer than 10 of its 5,000 pages,
//!   asserted below.
//! - Every other mode ranks sizes across the whole document
//!   (`parse::Headings::Ranked`) and its reader has read every page before it
//!   renders the first. Nothing a builder does can change that, so those modes
//!   keep the worker-thread-and-channel shape: construction still returns
//!   immediately and chunks still arrive one at a time, but the document is
//!   parsed in full behind them.
//!
//! Pagination is not itself an obstacle, though it looks like one: the end of
//! page 3 and the start of page 4 can be one paragraph, and chunking pages
//! *separately* turns `arxiv_1301.3781_word2vec` into 71 chunks instead of 66.
//! The builder does not chunk them separately — it is fed one continuous stream
//! of markdown and only ever finalises a chunk once the text after it has
//! arrived, which is what makes the output identical.
//!
//! **Output is byte-identical to batch in every mode**, because it is the same
//! code: the same `StructuralBuilder`, the same `ProseMerger` and
//! `pipeline::stamp`. Pinned across the corpus, for all seven modes and
//! including metadata, by `stream_matches_batch_for_every_mode` in
//! `tests/pdf_stream.rs`.
//!
//! wasm32 has no threads, so for the non-`default` modes the work happens on
//! the first `next()` and the chunks drain from there. `default` needs no
//! thread on any target. That is the split `streaming.mdx` already describes
//! under "Profiles, not runtimes".

use std::collections::VecDeque;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats::md::block_stream::{resume_point, BlockStream};
use crate::formats::md::common::MIN_CHUNK_CHARS;
use crate::formats::md::structural::{ProseMerger, StructuralBuilder};
use crate::formats::pipeline;

use super::markdown::PAGE_SEPARATOR;
use super::{author_block, parse};

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
    // Constructed only on wasm32 (native runs the same work on a worker thread
    // as `Threaded`), so native builds see it as never-constructed.
    #[allow(dead_code)]
    Deferred(Box<Deferred>),
    /// `default` mode with no threads to hand: a page is read only when a chunk
    /// is asked for. Native builds run the same [`Incremental`] on the worker
    /// instead, because the PDF [`parse::Reader`] holds `Rc`s and so cannot
    /// cross a thread — it is *built* on the worker and never moves.
    #[cfg(target_arch = "wasm32")]
    Incremental(Box<Incremental>),
    Draining(std::vec::IntoIter<Chunk>),
    Failed(Option<ChunkError>),
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

/// Chunks a PDF a page at a time, holding one page rather than the document.
///
/// **`default` only**, and that is not an oversight ([#87](TECH_DEBT.md)).
/// Every other mode ranks heading sizes across the whole document
/// ([`parse::Headings::Ranked`]), so its reader has already read every page
/// before it renders the first — there is nothing left for a resumable builder
/// to save. `default` is the documented per-page mode, and the only one where
/// the builder was the binding constraint.
///
/// Output is byte-identical to batch because every piece is the same code: the
/// same [`StructuralBuilder`] fed the same blocks, the same [`ProseMerger`],
/// and [`pipeline::stamp`] for the metadata.
struct Incremental {
    reader: parse::Reader,
    metadata: serde_json::Value,
    total_pages: usize,
    /// Page markdown not yet cut at a blank line.
    pending: String,
    blocks: BlockStream,
    builder: StructuralBuilder,
    merger: ProseMerger,
    queue: VecDeque<Chunk>,
    wrote_a_page: bool,
    drained: bool,
}

impl Incremental {
    fn open(bytes: &[u8]) -> Result<Incremental> {
        let reader =
            parse::Reader::open(bytes, parse::Headings::PerPage).map_err(ChunkError::Parse)?;
        let total_pages = reader.total_pages();
        Ok(Incremental {
            reader,
            // Knowable at open: `total_pages` is the page tree's length, not
            // something the content streams reveal. That is what lets a PDF
            // chunk carry its `document_metadata` before the document is read.
            metadata: serde_json::json!({ "source_type": "pdf", "total_pages": total_pages }),
            total_pages,
            pending: String::new(),
            blocks: BlockStream::new(),
            builder: StructuralBuilder::new(),
            merger: ProseMerger::new(),
            queue: VecDeque::new(),
            wrote_a_page: false,
            drained: false,
        })
    }

    /// Consume the buffered markdown, emitting whatever it completed.
    fn drain(&mut self, flush: bool) {
        let cut = if flush {
            self.pending.len()
        } else {
            resume_point(&self.pending)
        };
        if cut == 0 {
            return;
        }
        let rest = self.pending.split_off(cut);
        let ready = std::mem::replace(&mut self.pending, rest);
        // A byline table never straddles a cut — `PAGE_SEPARATOR` puts a blank
        // line and a rule between pages, `table_at` needs consecutive rows, and
        // a cut only ever lands after a blank line — so normalising each cut is
        // the same as normalising the whole document. Checked across the corpus
        // by `stream_matches_batch_for_every_mode`, which compares metadata as
        // well as content and would fail on any fixture where it is not.
        let normalized = author_block::normalize(&ready);
        for block in self.blocks.push(&normalized) {
            self.builder.advance(block);
        }
        if flush {
            for block in self.blocks.finish() {
                self.builder.advance(block);
            }
        }
        let finished = if flush {
            self.builder.finish()
        } else {
            self.builder.take()
        };
        for record in finished {
            if let Some(out) = self.merger.push(record, MIN_CHUNK_CHARS) {
                self.emit(out);
            }
        }
        if flush {
            if let Some(out) = self.merger.finish() {
                self.emit(out);
            }
        }
    }

    fn emit(&mut self, record: crate::formats::md::common::SpannedRecord) {
        let rec = pipeline::stamp(record, &self.metadata, None);
        self.queue.push_back(Chunk::new(
            rec.content,
            rec.content_type.as_str(),
            rec.metadata,
        ));
    }

    /// Read pages until at least one chunk is ready, or the document ends.
    fn pump(&mut self) -> Option<Result<Chunk>> {
        loop {
            if let Some(chunk) = self.queue.pop_front() {
                return Some(Ok(chunk));
            }
            if self.drained {
                return None;
            }
            match self.reader.next_page() {
                Some(page) => {
                    // A blank page contributes nothing, exactly as `parse` skips it.
                    if !page.trim().is_empty() {
                        if self.wrote_a_page {
                            self.pending.push_str(PAGE_SEPARATOR);
                        }
                        self.wrote_a_page = true;
                        self.pending.push_str(&page);
                    }
                    // Nothing may be emitted until the document is known to
                    // have text *somewhere*: batch throws the whole markdown
                    // away when it does not, because a scanned page still
                    // renders `![](…)` references and those are not text. So a
                    // text-less prefix is held rather than chunked — for a real
                    // document that is page one, and for a scanned one it is
                    // the image references, which are small.
                    if self.reader.has_text() {
                        self.drain(false);
                    }
                }
                None => {
                    self.drained = true;
                    if !self.reader.has_text() {
                        // Batch raises rather than returning nothing, and a
                        // stream that just ended would hide a scanned PDF.
                        // Nothing has been emitted, so this is still the first
                        // and only item the caller sees.
                        // Same diagnosis as the batch path, from the same
                        // function — this used to be a second copy of the
                        // "scanned or image-only" message, so an encrypted PDF
                        // was reported as encrypted in six modes and as a scan
                        // in `default`.
                        return Some(Err(super::diagnose(
                            self.total_pages,
                            self.reader.is_encrypted(),
                            &self.reader.take_skipped(),
                        )));
                    }
                    self.drain(true);
                }
            }
        }
    }
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
                #[cfg(target_arch = "wasm32")]
                Backend::Incremental(work) => return work.pump(),
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

/// `default`, page at a time.
///
/// The [`Incremental`] is built *inside* the worker and never leaves it: a PDF
/// [`parse::Reader`] caches fonts behind `Rc`, so it is not `Send`, and the
/// Python binding requires the stream it wraps to be. Only the receiver
/// crosses, exactly as the batch worker already did.
#[cfg(not(target_arch = "wasm32"))]
fn incremental(bytes: Vec<u8>) -> PdfChunkStream {
    let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_DEPTH);
    std::thread::spawn(move || {
        // The worker carries its own panic boundary. `dispatch`'s runs on the
        // *caller's* thread and cannot see a spawned worker, so a panic here
        // simply unwound, dropped `tx`, and turned `rx.recv().ok()` into a clean
        // end-of-stream — mid-document, after the consumer had already taken
        // correct chunks, with no error anywhere. Truncation that looks like
        // completion (TECH_DEBT F7).
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut work = Incremental::open(&bytes)?;
            // The bounded channel is what keeps this incremental in practice:
            // the worker blocks once the consumer is CHANNEL_DEPTH chunks
            // behind, so pages are read at the rate they are consumed rather
            // than all at once.
            while let Some(item) = work.pump() {
                let failed = item.is_err();
                if tx.send(item).is_err() || failed {
                    break;
                }
            }
            Ok::<(), ChunkError>(())
        }));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = tx.send(Err(error));
            }
            Err(payload) => {
                let msg = crate::error::panic_message(payload);
                let _ = tx.send(Err(ChunkError::Parse(format!(
                    "internal parser panic: {msg}"
                ))));
            }
        }
    });
    PdfChunkStream {
        backend: Backend::Threaded(rx),
    }
}

/// No threads on wasm, so the same work runs on the calling thread, one page
/// per `next()`.
#[cfg(target_arch = "wasm32")]
fn incremental(bytes: Vec<u8>) -> PdfChunkStream {
    match Incremental::open(&bytes) {
        Ok(work) => PdfChunkStream {
            backend: Backend::Incremental(Box::new(work)),
        },
        Err(error) => PdfChunkStream {
            backend: Backend::Failed(Some(error)),
        },
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
    // `default` is the one mode whose reader is already page-at-a-time, so it
    // is the one mode a resumable builder can make incremental. Everything else
    // ranks headings across the document and has read it all before rendering
    // page one.
    if mode == "default" {
        return incremental(bytes);
    }

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
        std::thread::spawn(move || {
            // Same boundary as the incremental worker above, for the same
            // reason: a panic on this thread is invisible to `dispatch`.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for chunk in work.run()? {
                    // A closed receiver means the consumer stopped early — an
                    // ordinary outcome, not a failure.
                    if tx.send(Ok(chunk)).is_err() {
                        break;
                    }
                }
                Ok::<(), ChunkError>(())
            }));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = tx.send(Err(error));
                }
                Err(payload) => {
                    let msg = crate::error::panic_message(payload);
                    let _ = tx.send(Err(ChunkError::Parse(format!(
                        "internal parser panic: {msg}"
                    ))));
                }
            }
        });
        Backend::Threaded(rx)
    };

    #[cfg(target_arch = "wasm32")]
    let backend = Backend::Deferred(Box::new(work));

    PdfChunkStream { backend }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        [env!("CARGO_MANIFEST_DIR"), "..", "test_files", "pdf", name]
            .iter()
            .collect()
    }

    /// The whole point of [#87](TECH_DEBT.md), stated as a number.
    ///
    /// `stream_matches_batch_for_every_mode` proves the output is unchanged,
    /// which a stream that quietly parsed the whole document would also pass.
    /// This is the part that would not: a chunk must cost the pages it came
    /// from, not the document it came from.
    #[test]
    fn the_first_chunk_costs_a_few_pages_not_the_whole_document() {
        let path = fixture("sample-5000-page.pdf");
        if !path.exists() {
            return;
        }
        let bytes = std::fs::read(&path).expect("read");
        let mut work = Incremental::open(&bytes).expect("open");
        assert_eq!(
            work.reader.pages_rendered(),
            0,
            "opening a stream must render nothing"
        );
        assert_eq!(work.total_pages, 5_000, "fixture changed");

        let first = work.pump().expect("a chunk").expect("chunked");
        assert!(!first.content.is_empty());
        let pages = work.reader.pages_rendered();
        assert!(
            pages < 10,
            "the first chunk cost {pages} pages of {}",
            work.total_pages
        );
    }

    /// A text-less PDF must still raise, and must not raise until it is certain
    /// — the markdown of a scanned page is image references, which batch throws
    /// away rather than chunking.
    #[test]
    fn a_scanned_document_raises_instead_of_chunking_its_image_references() {
        let path = fixture("large-doc.pdf");
        if !path.exists() {
            return;
        }
        let bytes = std::fs::read(&path).expect("read");
        let mut work = Incremental::open(&bytes).expect("open");
        match work.pump() {
            Some(Err(error)) => assert!(error.to_string().contains("no extractable text")),
            other => panic!("expected an error, got {:?}", other.map(|r| r.is_ok())),
        }
    }
}
