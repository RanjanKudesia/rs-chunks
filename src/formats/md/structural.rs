use serde_json::{json, Value};

use super::common::{
    classify_prose, extend_span, extract_heading_text, heading_level, parse_markdown_blocks,
    split_at_sentences, strip_block_content, union_span, BlockSpan, ChunkRecordInput, ContentType,
    MdBlock, MdBlockType, SpannedRecord, MAX_CHUNK_CHARS, MIN_CHUNK_CHARS,
};

// ── Prose helpers ─────────────────────────────────────────────────────────────

fn is_prose(content_type: ContentType) -> bool {
    matches!(
        content_type,
        ContentType::PlainParagraph
            | ContentType::ShortDisconnectedParagraph
            | ContentType::LongSingleParagraph
    )
}

/// Absorbs a short prose chunk into the one before it.
///
/// This was a post-pass over the finished `Vec`, and [#87](TECH_DEBT.md) listed
/// it as one of the things stopping a builder resuming. It never was: it only
/// ever looked at `result.last_mut()`, so **one chunk of lookback** is all it
/// needs. Holding a single chunk back until the next has declined to merge into
/// it gives byte-identical output and lets everything before it stream.
#[derive(Default)]
pub(crate) struct ProseMerger {
    held: Option<SpannedRecord>,
}

impl ProseMerger {
    pub fn new() -> ProseMerger {
        ProseMerger::default()
    }

    /// Offer a chunk; returns the one before it, if that can no longer grow.
    pub fn push(&mut self, next: SpannedRecord, min_chars: usize) -> Option<SpannedRecord> {
        let soft_max = MAX_CHUNK_CHARS + min_chars;
        let SpannedRecord {
            record: next,
            blocks,
        } = next;
        if is_prose(next.content_type) && next.content.len() < min_chars {
            if let Some(previous) = self.held.as_mut() {
                let prev = &mut previous.record;
                if is_prose(prev.content_type) && prev.content.len() + next.content.len() < soft_max
                {
                    prev.content = format!("{}\n{}", prev.content, next.content)
                        .trim()
                        .to_string();
                    prev.content_type = classify_prose(&prev.content);
                    // The merged chunk now covers both sources' blocks.
                    previous.blocks = union_span(previous.blocks, blocks);
                    return None;
                }
            }
        }
        self.held.replace(SpannedRecord::spanning(next, blocks))
    }

    pub fn finish(&mut self) -> Option<SpannedRecord> {
        self.held.take()
    }
}

fn merge_short_prose(chunks: Vec<SpannedRecord>, min_chars: usize) -> Vec<SpannedRecord> {
    let mut merger = ProseMerger::new();
    let mut result: Vec<SpannedRecord> = chunks
        .into_iter()
        .filter_map(|chunk| merger.push(chunk, min_chars))
        .collect();
    result.extend(merger.finish());
    result
}

fn md_metadata(section_heading: Option<String>, section_level: u8) -> Value {
    json!({
        "section_heading": section_heading,
        "section_level":   section_level,
        "document_metadata": { "source_type": "md" }
    })
}

// ── The builder, as resumable state ──────────────────────────────────────────

/// The `structural`/`default` builder.
///
/// A state machine rather than a fold over a whole `Vec<MdBlock>`, so a caller
/// holding only part of a document can still be handed the chunks that part
/// completed ([#87](TECH_DEBT.md)). Everything that must survive a resume is a
/// field: the heading in force, its level, and the prose buffered so far.
///
/// The batch entry point below is this fed every block at once, so there is one
/// implementation and the batch and streaming paths cannot drift.
///
/// `total_input_blocks` is deliberately absent. It is the whole document's
/// block count, which a builder resumed at block *n* cannot know — `structural`
/// never emitted it, and [`super::section`], which does, is not resumable for
/// exactly that reason.
#[derive(Default)]
pub(crate) struct StructuralBuilder {
    ready: Vec<SpannedRecord>,
    heading: Option<String>,
    section_level: u8,
    prose_parts: Vec<String>,
    prose_len: usize,
    /// The blocks the buffered prose came from, flushed with it.
    prose_span: Option<BlockSpan>,
}

impl StructuralBuilder {
    pub fn new() -> StructuralBuilder {
        StructuralBuilder::default()
    }

    /// Take the chunks completed so far.
    pub fn take(&mut self) -> Vec<SpannedRecord> {
        std::mem::take(&mut self.ready)
    }

    /// Flush the buffered prose and take everything outstanding.
    pub fn finish(&mut self) -> Vec<SpannedRecord> {
        self.flush_prose();
        self.take()
    }

    fn flush_prose(&mut self) {
        if self.prose_parts.is_empty() {
            return;
        }
        let content = self.prose_parts.join("\n").trim().to_string();
        self.prose_parts.clear();
        self.prose_len = 0;
        let blocks = self.prose_span.take();
        if content.is_empty() {
            return;
        }
        self.ready.push(SpannedRecord::spanning(
            ChunkRecordInput {
                content_type: classify_prose(&content),
                content,
                metadata: md_metadata(self.heading.clone(), self.section_level),
            },
            blocks,
        ));
    }

    fn push_at(&mut self, content_type: ContentType, content: String, index: usize) {
        let metadata = md_metadata(self.heading.clone(), self.section_level);
        self.ready.push(SpannedRecord::at(
            ChunkRecordInput {
                content_type,
                content,
                metadata,
            },
            index,
        ));
    }

    /// Feed one block.
    pub fn advance(&mut self, block: MdBlock) {
        match block.block_type {
            MdBlockType::Heading => {
                self.flush_prose();
                let heading_text = extract_heading_text(&block.content);
                let level = heading_level(&block.content);
                self.heading = Some(heading_text.clone());
                self.section_level = level;
                // A heading names itself, so its own `section_heading` is null.
                self.ready.push(SpannedRecord::at(
                    ChunkRecordInput {
                        content_type: ContentType::HeadingSection,
                        content: heading_text,
                        metadata: json!({
                            "section_heading": serde_json::Value::Null,
                            "section_level": level,
                            "document_metadata": { "source_type": "md" }
                        }),
                    },
                    block.index,
                ));
            }
            MdBlockType::Code => {
                self.flush_prose();
                self.push_at(ContentType::CodeBlock, block.content, block.index);
            }
            MdBlockType::Table => {
                self.flush_prose();
                self.push_at(ContentType::Table, block.content, block.index);
            }
            MdBlockType::List => {
                self.flush_prose();
                let clean = strip_block_content(&block.content, true);
                if !clean.is_empty() {
                    self.push_at(ContentType::BulletNumberedList, clean, block.index);
                }
            }
            MdBlockType::Paragraph => self.advance_paragraph(&block),
        }
    }

    fn advance_paragraph(&mut self, block: &MdBlock) {
        let clean = strip_block_content(&block.content, false);
        if clean.is_empty() {
            return;
        }
        let sub_blocks = if clean.len() > MAX_CHUNK_CHARS {
            split_at_sentences(&clean, MAX_CHUNK_CHARS)
        } else {
            vec![clean]
        };
        for sub in sub_blocks {
            let add = sub.len() + 1;
            if self.prose_len + add > MAX_CHUNK_CHARS && !self.prose_parts.is_empty() {
                self.flush_prose();
            }
            self.prose_len += add;
            self.prose_parts.push(sub);
            extend_span(&mut self.prose_span, block.index);
        }
    }
}

// ── Core build function ───────────────────────────────────────────────────────

pub fn build_chunks_from_md_bytes(bytes: &[u8]) -> Result<Vec<SpannedRecord>, String> {
    let text = crate::text_encoding::decode_text(bytes).0;

    // Empty input is not a failure. A blank or whitespace-only document parsed
    // perfectly well; it simply has nothing to chunk, so it returns `[]` like
    // docx/ppt/xlsx always have (TECH_DEBT T6). Reserving errors for genuine
    // parse failures is also what lets `epub::extract` stop swallowing them.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = StructuralBuilder::new();
    for block in parse_markdown_blocks(&text) {
        builder.advance(block);
    }
    let chunks = merge_short_prose(builder.finish(), MIN_CHUNK_CHARS);

    // A document that parsed cleanly but holds no extractable text is EMPTY,
    // not broken: return an empty list rather than an error. Raising here
    // conflated "nothing to chunk" with "could not read this file", and it was
    // inconsistent — docx/ppt/xlsx already returned `[]` for the same condition
    // while txt/html/md raised (TECH_DEBT T6). Structural invalidity still
    // errors: a PPTX with no slides, or a PDF with no text layer, both raise a
    // typed error carrying a remedy.
    Ok(chunks)
}

// ── PyO3 entry point ──────────────────────────────────────────────────────────
