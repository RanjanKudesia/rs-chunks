/// Shared types, constants, block parser, and helper functions used by every
/// MD chunking strategy.
// Re-export shared utilities so strategy files can keep importing from super::common.
pub use crate::shared::{
    has_keyword_overlap, split_at_sentences, tokenize_keywords,
};

/// Classify prose length into a ContentType variant.
pub fn classify_prose(text: &str) -> ContentType {
    if text.len() > 900 {
        ContentType::LongSingleParagraph
    } else if text.len() < 90 {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

/// Split `text` at `\n\n` paragraph boundaries so no piece exceeds `max_chars`.
/// Always returns at least one element even when no split point exists.
pub fn split_at_paragraph_boundary(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.trim().to_string()];
    }
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();
    for part in text.split("\n\n") {
        if current.is_empty() {
            current.push_str(part);
        } else if current.len() + 2 + part.len() <= max_chars {
            current.push_str("\n\n");
            current.push_str(part);
        } else {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                result.push(trimmed);
            }
            current = part.to_string();
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }
    if result.is_empty() {
        result.push(text.trim().to_string());
    }
    result
}

/// [`split_at_paragraph_boundary`], but each output part also reports the blocks
/// it was built from.
///
/// The text is joined and split exactly as the untagged version does — the
/// strings it returns are byte-for-byte the same — and attribution is done by
/// *offset* into the joined text. Re-splitting each part separately would be
/// simpler and wrong: a part ending in a newline joins to `"a\n\n\nb"`, which
/// splits differently from the parts it came from.
pub fn split_at_paragraph_boundary_spanned(
    parts: &[(String, usize)],
    max_chars: usize,
) -> Vec<(String, Option<BlockSpan>)> {
    let joined = parts.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>().join("\n\n");

    // Where each source part sits in the joined text, so an offset can name it.
    let mut sources: Vec<(std::ops::Range<usize>, usize)> = Vec::with_capacity(parts.len());
    let mut at = 0usize;
    for (content, block) in parts {
        sources.push((at..at + content.len(), *block));
        at += content.len() + 2; // the "\n\n" the join inserts
    }
    let span_of = |range: std::ops::Range<usize>| {
        let mut span = None;
        for (source, block) in &sources {
            // Touching counts: a part that contributed any character to this
            // output part is one of its sources.
            if source.start < range.end && range.start < source.end.max(source.start + 1) {
                extend_span(&mut span, *block);
            }
        }
        span
    };

    if joined.len() <= max_chars {
        return vec![(joined.trim().to_string(), span_of(0..joined.len().max(1)))];
    }

    let mut result: Vec<(String, Option<BlockSpan>)> = Vec::new();
    let mut current = String::new();
    let mut start = 0usize;
    let mut end = 0usize;
    let mut offset = 0usize;
    for part in joined.split("\n\n") {
        let here = offset..offset + part.len();
        offset += part.len() + 2;
        if current.is_empty() {
            current.push_str(part);
            start = here.start;
            end = here.end;
        } else if current.len() + 2 + part.len() <= max_chars {
            current.push_str("\n\n");
            current.push_str(part);
            end = here.end;
        } else {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                result.push((trimmed, span_of(start..end)));
            }
            current = part.to_string();
            start = here.start;
            end = here.end;
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push((trimmed, span_of(start..end)));
    }
    if result.is_empty() {
        result.push((joined.trim().to_string(), span_of(0..joined.len().max(1))));
    }
    result
}

pub const MAX_CHUNK_CHARS: usize = 1200;
pub const MIN_CHUNK_CHARS: usize = 350;

// ── Content types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    PlainParagraph,
    HeadingSection,
    BulletNumberedList,
    Table,
    CodeBlock,
    LongSingleParagraph,
    ShortDisconnectedParagraph,
    Semantic,
    Section,
    SlidingWindow,
    Sentence,
    PageAware,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::PlainParagraph => "plain_paragraph",
            ContentType::HeadingSection => "heading",
            ContentType::BulletNumberedList => "bullet_list",
            ContentType::Table => "table",
            ContentType::CodeBlock => "code_block",
            ContentType::LongSingleParagraph => "long_single_paragraph",
            ContentType::ShortDisconnectedParagraph => "short_disconnected_paragraph",
            ContentType::Semantic => "semantic",
            ContentType::Section => "section",
            ContentType::SlidingWindow => "sliding_window",
            ContentType::Sentence => "sentence",
            ContentType::PageAware => "page_aware",
        }
    }
}

// ── Block model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdBlockType {
    Heading,
    Paragraph,
    Code,
    Table,
    List,
}

#[derive(Debug, Clone)]
pub struct MdBlock {
    pub block_type: MdBlockType,
    pub content: String,
    /// Position in the document's block list, assigned once by
    /// [`parse_markdown_blocks`]. Carrying it on the block means it travels
    /// wherever the block does, including through the builders that buffer
    /// blocks and flush them later.
    pub index: usize,
}

// ── Chunk output ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChunkRecordInput {
    pub content_type: ContentType,
    pub content: String,
    pub metadata: serde_json::Value,
}

/// The inclusive first and last markdown block a chunk was built from.
pub type BlockSpan = (usize, usize);

/// A chunk together with where it came from.
///
/// The span is **internal**: it never reaches a caller as itself.
/// [`crate::formats::pipeline`] translates it into `record_range` for the
/// formats that have records — `.json`, `.jsonl`, `.ndjson` — and drops it for
/// everything else, which is why tracking it changes no other format's
/// metadata. It rides here rather than on [`ChunkRecordInput`] because that
/// struct is built at 89 sites across five format families, none of which have
/// blocks to point at.
#[derive(Debug, Clone)]
pub struct SpannedRecord {
    pub record: ChunkRecordInput,
    pub blocks: Option<BlockSpan>,
}

impl SpannedRecord {
    /// A chunk whose origin is a single block.
    pub fn at(record: ChunkRecordInput, index: usize) -> SpannedRecord {
        SpannedRecord { record, blocks: Some((index, index)) }
    }

    /// A chunk built from a span the caller accumulated.
    pub fn spanning(record: ChunkRecordInput, blocks: Option<BlockSpan>) -> SpannedRecord {
        SpannedRecord { record, blocks }
    }
}

/// Grow a span to include one more block.
pub fn extend_span(span: &mut Option<BlockSpan>, index: usize) {
    *span = Some(match *span {
        Some((first, last)) => (first.min(index), last.max(index)),
        None => (index, index),
    });
}

/// The smallest span covering both, for a builder that merges two chunks.
pub fn union_span(left: Option<BlockSpan>, right: Option<BlockSpan>) -> Option<BlockSpan> {
    match (left, right) {
        (Some((a, b)), Some((c, d))) => Some((a.min(c), b.max(d))),
        (only, None) | (None, only) => only,
    }
}

// ── Heading helpers ───────────────────────────────────────────────────────────

/// Returns the ATX level (1-6) or setext level of a heading block.
pub fn heading_level(content: &str) -> u8 {
    let first = content.lines().next().unwrap_or("").trim();
    if first.starts_with('#') {
        return (first.chars().take_while(|c| *c == '#').count() as u8).min(6);
    }
    // setext: second line is all '=' (H1) or all '-' (H2)
    let mut lines = content.lines();
    lines.next();
    if let Some(ul) = lines.next() {
        let ul = ul.trim();
        if ul.len() >= 2 && ul.chars().all(|c| c == '=') {
            return 1;
        }
        if ul.len() >= 2 && ul.chars().all(|c| c == '-') {
            return 2;
        }
    }
    1
}

/// Strips the ATX `#` prefix (or setext underline) and returns plain text.
pub fn extract_heading_text(heading_block: &str) -> String {
    let lines: Vec<&str> = heading_block.lines().collect();
    if lines.is_empty() {
        return heading_block.trim().to_string();
    }
    let first = lines[0].trim();
    if first.starts_with('#') {
        return first.trim_start_matches('#').trim().to_string();
    }
    first.to_string()
}

/// Maintains an ancestor heading stack: retains entries shallower than `level`
/// then pushes the new heading.
pub fn update_heading_stack(stack: &mut Vec<(u8, String)>, level: u8, text: String) {
    stack.retain(|(l, _)| *l < level);
    stack.push((level, text));
}

pub fn current_section_heading(stack: &[(u8, String)]) -> Option<String> {
    stack.last().map(|(_, t)| t.clone())
}

pub fn current_section_level(stack: &[(u8, String)]) -> u8 {
    stack.last().map(|(l, _)| *l).unwrap_or(0)
}

pub fn heading_path_strings(stack: &[(u8, String)]) -> Vec<String> {
    stack.iter().map(|(_, t)| t.clone()).collect()
}

// ── Markdown block parser ─────────────────────────────────────────────────────

/// Split any block that is too large to ever be one chunk.
///
/// Paragraphs were bounded downstream, but lists, tables and code blocks were
/// carried whole into every mode. A TopoJSON document rendered as a single
/// bullet list produced one 764,655-character chunk — orders of magnitude past
/// what an embedding model accepts, which defeats the point of chunking.
///
/// Doing it here rather than in each mode means all seven get it, and get it
/// the same way. A table repeats its header and separator in each part so every
/// part stands alone; lines are never broken mid-way, so no list item or table
/// row is mangled.
fn bound_block_size(blocks: Vec<MdBlock>) -> Vec<MdBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        // Prose is divisible, so a line still over the cap after the line
        // split is cut at its sentences rather than emitted whole (#68).
        // A rendered JSON record arrives as a `List` of one enormous line —
        // `elastic_rag_dataset.ndjson` record 284 is 140,079 characters — and
        // the #45 precedent is that a bounded chunk beats an intact one.
        //
        // `Table` and `Code` are excluded: a table's rows carry its structure
        // and it is documented as "kept whole", and a code block's lines are
        // its meaning.
        let divisible = matches!(
            block.block_type,
            MdBlockType::Paragraph | MdBlockType::List | MdBlockType::Heading
        );
        let parts = if divisible {
            crate::shared::split_block_on_lines_and_sentences(&block.content, MAX_CHUNK_CHARS)
        } else {
            let repeat_prefix = if block.block_type == MdBlockType::Table { 2 } else { 0 };
            crate::shared::split_block_on_lines(&block.content, MAX_CHUNK_CHARS, repeat_prefix)
        };
        for part in parts {
            out.push(MdBlock {
                block_type: block.block_type,
                content: part,
                index: 0,
            });
        }
    }
    out
}

pub fn parse_markdown_blocks(text: &str) -> Vec<MdBlock> {
    parse_blocks_from(text, 0)
}

/// Parse a *span* of a document, numbering its blocks from `first_index`.
///
/// The whole-document entry point is this with `first_index` of 0. It is
/// separate because block numbering used to be `enumerate()` over the finished
/// `Vec`, which is a whole-document operation and one of the three things that
/// stopped a builder resuming mid-document ([#87](TECH_DEBT.md)).
pub(crate) fn parse_blocks_from(text: &str, first_index: usize) -> Vec<MdBlock> {
    let mut blocks = bound_block_size(parse_markdown_blocks_unbounded(text));
    // Numbered after bounding, because bounding is what splits an oversized
    // block into several and the builders see the result.
    for (offset, block) in blocks.iter_mut().enumerate() {
        block.index = first_index + offset;
    }
    blocks
}

fn parse_markdown_blocks_unbounded(text: &str) -> Vec<MdBlock> {
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut lines: Vec<&str> = text.lines().collect();

    if text.ends_with('\n') {
        lines.push("");
    }

    let mut i = 0;
    let mut paragraph: Vec<String> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let mut table: Vec<String> = Vec::new();
    let mut code: Vec<String> = Vec::new();
    let mut in_code_fence = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_end();
        let compact = trimmed.trim();

        if in_code_fence {
            code.push(trimmed.to_string());
            if compact.starts_with("```") {
                blocks.push(MdBlock {
                    block_type: MdBlockType::Code,
                    content: code.join("\n").trim().to_string(),
                index: 0,
                });
                code.clear();
                in_code_fence = false;
            }
            i += 1;
            continue;
        }

        if compact.starts_with("```") {
            flush_text_blocks(&mut blocks, &mut paragraph, &mut list, &mut table);
            in_code_fence = true;
            code.push(trimmed.to_string());
            i += 1;
            continue;
        }

        if compact.is_empty() {
            flush_text_blocks(&mut blocks, &mut paragraph, &mut list, &mut table);
            i += 1;
            continue;
        }

        if is_horizontal_rule(compact) {
            flush_text_blocks(&mut blocks, &mut paragraph, &mut list, &mut table);
            i += 1;
            continue;
        }

        if i + 1 < lines.len() && is_setext_underline(lines[i + 1].trim()) && !compact.contains('|')
        {
            flush_text_blocks(&mut blocks, &mut paragraph, &mut list, &mut table);
            blocks.push(MdBlock {
                block_type: MdBlockType::Heading,
                content: format!("{}\n{}", compact, lines[i + 1].trim()),
                index: 0,
            });
            i += 2;
            continue;
        }

        if is_atx_heading(compact) {
            flush_text_blocks(&mut blocks, &mut paragraph, &mut list, &mut table);
            blocks.push(MdBlock {
                block_type: MdBlockType::Heading,
                content: compact.to_string(),
                index: 0,
            });
            i += 1;
            continue;
        }

        if is_list_item_line(compact) {
            if !table.is_empty() {
                flush_table(&mut blocks, &mut table);
            }
            if !paragraph.is_empty() {
                flush_paragraph(&mut blocks, &mut paragraph);
            }
            // Keep the leading whitespace: a list's nesting is carried entirely
            // by its indentation, and pushing the trimmed line here destroyed it
            // before any consumer could see it. `trimmed` is the line with only
            // trailing whitespace removed. (#27)
            list.push(trimmed.to_string());
            i += 1;
            continue;
        }

        if looks_like_table_row(compact) {
            if !list.is_empty() {
                flush_list(&mut blocks, &mut list);
            }
            if !paragraph.is_empty() {
                flush_paragraph(&mut blocks, &mut paragraph);
            }
            table.push(compact.to_string());
            i += 1;
            continue;
        }

        if !list.is_empty() {
            flush_list(&mut blocks, &mut list);
        }
        if !table.is_empty() {
            flush_table(&mut blocks, &mut table);
        }
        paragraph.push(compact.to_string());
        i += 1;
    }

    flush_text_blocks(&mut blocks, &mut paragraph, &mut list, &mut table);

    if !code.is_empty() {
        blocks.push(MdBlock {
            block_type: MdBlockType::Code,
            content: code.join("\n").trim().to_string(),
                index: 0,
        });
    }

    blocks
}

pub fn flush_text_blocks(
    blocks: &mut Vec<MdBlock>,
    paragraph: &mut Vec<String>,
    list: &mut Vec<String>,
    table: &mut Vec<String>,
) {
    if !paragraph.is_empty() {
        flush_paragraph(blocks, paragraph);
    }
    if !list.is_empty() {
        flush_list(blocks, list);
    }
    if !table.is_empty() {
        flush_table(blocks, table);
    }
}

pub fn flush_paragraph(blocks: &mut Vec<MdBlock>, paragraph: &mut Vec<String>) {
    let content = paragraph.join("\n").trim().to_string();
    paragraph.clear();
    if !content.is_empty() {
        blocks.push(MdBlock {
            block_type: MdBlockType::Paragraph,
            content,
                index: 0,
        });
    }
}

pub fn flush_list(blocks: &mut Vec<MdBlock>, list: &mut Vec<String>) {
    let content = list.join("\n").trim().to_string();
    list.clear();
    if !content.is_empty() {
        blocks.push(MdBlock {
            block_type: MdBlockType::List,
            content,
                index: 0,
        });
    }
}

/// A GFM delimiter row: `| --- | :---: |`. Its presence is what makes the
/// preceding line a table header rather than a sentence containing pipes.
fn is_table_delimiter_row(line: &str) -> bool {
    let cells: Vec<&str> = line
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(str::trim)
        .collect();
    !cells.is_empty()
        && cells.iter().all(|c| {
            !c.is_empty()
                && c.chars().all(|ch| ch == '-' || ch == ':')
                && c.contains('-')
        })
}

/// True when the collected rows really are a table.
///
/// `looks_like_table_row` only counts pipes, so a sentence like
/// "run foo | grep bar | wc -l" was flushed as `content_type: "table"`. GFM
/// requires a delimiter row, and a hand-written table without one still gives
/// itself away by starting and ending every row with a pipe. Anything else is
/// prose that happens to contain a pipe.
fn rows_form_a_table(rows: &[String]) -> bool {
    if rows.len() >= 2 && is_table_delimiter_row(&rows[1]) {
        return true;
    }
    rows.len() >= 2
        && rows
            .iter()
            .all(|r| r.trim().starts_with('|') && r.trim().ends_with('|'))
}

pub fn flush_table(blocks: &mut Vec<MdBlock>, table: &mut Vec<String>) {
    let is_table = rows_form_a_table(table);
    let content = table.join("\n").trim().to_string();
    table.clear();
    if content.is_empty() {
        return;
    }
    blocks.push(MdBlock {
        // Pipes alone do not make a table. Misclassifying prose as one is not
        // content loss, but `content_type` is a documented field consumers
        // branch on, so it has to be right. (#29)
        block_type: if is_table {
            MdBlockType::Table
        } else {
            MdBlockType::Paragraph
        },
        content,
        index: 0,
    });
}

// ── Block detection ───────────────────────────────────────────────────────────

pub fn is_atx_heading(line: &str) -> bool {
    if line.is_empty() || !line.starts_with('#') {
        return false;
    }
    let hash_count = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hash_count) && line.chars().nth(hash_count) == Some(' ')
}

pub fn is_setext_underline(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3 && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-'))
}

pub fn looks_like_table_row(line: &str) -> bool {
    if !line.contains('|') {
        return false;
    }
    let non_empty_cells = line
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .count();
    non_empty_cells >= 2
        && line
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|cell| !cell.starts_with('[') || (cell.contains(']') && !cell.contains("](#")))
}

pub fn is_horizontal_rule(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_')
}

pub fn is_list_item_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    if t.starts_with("[x] ") || t.starts_with("[ ] ") || t.starts_with("[X] ") {
        return true;
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 4 {
        let rest = &t[digits.len()..];
        if matches!(rest.chars().next(), Some('.') | Some(')')) && rest.len() > 1 {
            return true;
        }
    }
    false
}

// ── Text cleaning ─────────────────────────────────────────────────────────────

/// Widest indent kept, so a pathological input cannot blow up a chunk.
const MAX_LIST_INDENT: usize = 24;

/// Leading whitespace of `line`, in spaces, with a tab counted as four.
fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

pub fn strip_block_content(text: &str, strip_bullets: bool) -> String {
    text.lines()
        .map(|line| {
            // A list's nesting lives entirely in its indentation, and trimming
            // every line flattened it: a three-level list came out as seven
            // unrelated lines. That hurts most on JSON, where nested objects are
            // rendered as indented bullets and the structure is the meaning.
            // Bullet markers are still stripped — the indent alone carries the
            // hierarchy, without putting Markdown syntax back into the content.
            let indent = if strip_bullets {
                indent_width(line).min(MAX_LIST_INDENT)
            } else {
                0
            };
            let mut l = line.trim_start();
            while l.starts_with('>') {
                l = l.trim_start_matches('>').trim_start();
            }
            let l = if strip_bullets {
                strip_list_prefix(l)
            } else {
                l
            };
            let body = strip_inline(l).trim().to_string();
            if body.is_empty() || indent == 0 {
                body
            } else {
                format!("{}{}", " ".repeat(indent), body)
            }
        })
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn strip_list_prefix(line: &str) -> &str {
    let t = line.trim_start();
    for prefix in &["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    for prefix in &["[x] ", "[X] ", "[ ] "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 4 {
        let rest = &t[digits.len()..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return rest[2..].trim_start();
        }
    }
    t
}

/// CommonMark forbids `_` from opening or closing emphasis *inside* a word:
/// a `_` run flanked by alphanumerics on both sides is literal text. Without
/// this rule `snake_case_word` renders as `snakecaseword`, which is wrong for
/// Markdown itself and doubly wrong for the formats that assemble Markdown from
/// a source that never had emphasis at all (ODF, `.msg`, PDF).
///
/// `*` is deliberately NOT subject to this rule — CommonMark does allow
/// intraword `*` emphasis, so `star*inside*word` really is emphasis.
fn underscore_is_intraword(chars: &[char], run_start: usize, run_len: usize) -> bool {
    let before = run_start.checked_sub(1).map(|j| chars[j]);
    let after = chars.get(run_start + run_len).copied();
    matches!(before, Some(c) if c.is_alphanumeric())
        && matches!(after, Some(c) if c.is_alphanumeric())
}

/// `Some(alt)` when `tag` is an `<img …>` start tag; the alt text may be empty.
///
/// Only `alt`/`title` are read — `src` is a path, not something a reader wants
/// in the text.
fn html_img_alt(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let name = lower.trim_start_matches('<').trim_start();
    if !(name.starts_with("img ") || name.starts_with("img>") || name.starts_with("img/")) {
        return None;
    }
    for attr in ["alt", "title"] {
        for quote in ['"', '\''] {
            let needle = format!("{attr}={quote}");
            if let Some(start) = lower.find(&needle) {
                let from = start + needle.len();
                if let Some(len) = tag[from..].find(quote) {
                    return Some(tag[from..from + len].to_string());
                }
            }
        }
    }
    Some(String::new())
}

pub fn strip_inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;

    while i < n {
        match chars[i] {
            '\\' if i + 1 < n => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '<' if i + 1 < n && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '/') => {
                let tag_start = i;
                while i < n && chars[i] != '>' {
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
                // Raw <img> is as much an image reference as ![](…) is, and
                // Markdown allows it — Jupyter notebooks use it constantly. It
                // was being swallowed with the rest of the inline HTML. (#41/#42)
                let tag: String = chars[tag_start..i].iter().collect();
                if let Some(alt) = html_img_alt(&tag) {
                    match alt.trim() {
                        "" => out.push_str("[Image]"),
                        a => out.push_str(&format!("[Image: {a}]")),
                    }
                }
            }
            '!' if i + 1 < n && chars[i + 1] == '[' => {
                i += 2;
                let alt_start = i;
                while i < n && chars[i] != ']' {
                    i += 1;
                }
                let alt: String = chars[alt_start..i].iter().collect();
                if i < n {
                    i += 1;
                }
                if i < n && chars[i] == '(' {
                    while i < n && chars[i] != ')' {
                        i += 1;
                    }
                    if i < n {
                        i += 1;
                    }
                }
                // An image with no alt text used to leave NOTHING behind, so
                // every image reference vanished from the chunks even though
                // get_markdown showed it (#41) — and a notebook made only of
                // image references produced zero chunks at all (#42). Use the
                // same placeholder docx and pptx already use, so a reader can
                // tell an image was there.
                match alt.trim() {
                    "" => out.push_str("[Image]"),
                    a => out.push_str(&format!("[Image: {a}]")),
                }
            }
            '[' if i + 1 < n && chars[i + 1] == '^' => {
                while i < n && chars[i] != ']' {
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            '[' => {
                i += 1;
                let text_start = i;
                let mut depth = 1i32;
                while i < n && depth > 0 {
                    if chars[i] == '[' {
                        depth += 1;
                    } else if chars[i] == ']' {
                        depth -= 1;
                    }
                    if depth > 0 {
                        i += 1;
                    }
                }
                let inner: String = chars[text_start..i].iter().collect();
                if i < n {
                    i += 1;
                }
                if i < n && chars[i] == '(' {
                    while i < n && chars[i] != ')' {
                        i += 1;
                    }
                    if i < n {
                        i += 1;
                    }
                } else if i < n && chars[i] == '[' {
                    while i < n && chars[i] != ']' {
                        i += 1;
                    }
                    if i < n {
                        i += 1;
                    }
                }
                out.push_str(&strip_inline(&inner));
            }
            '~' if i + 1 < n && chars[i + 1] == '~' => {
                i += 2;
                let start = i;
                while i + 1 < n && !(chars[i] == '~' && chars[i + 1] == '~') {
                    i += 1;
                }
                let inner: String = chars[start..i].iter().collect();
                if i + 1 < n {
                    i += 2;
                }
                out.push_str(&strip_inline(&inner));
            }
            '*' => {
                if i + 2 < n && chars[i + 1] == '*' && chars[i + 2] == '*' {
                    let open = i + 3;
                    if let Some(pos) = find_md_marker(&chars, open, &['*', '*', '*']) {
                        let inner: String = chars[open..pos].iter().collect();
                        i = pos + 3;
                        out.push_str(&strip_inline(&inner));
                    } else {
                        out.push('*');
                        i += 1;
                    }
                } else if i + 1 < n && chars[i + 1] == '*' {
                    let open = i + 2;
                    if let Some(pos) = find_md_marker(&chars, open, &['*', '*']) {
                        let inner: String = chars[open..pos].iter().collect();
                        i = pos + 2;
                        out.push_str(&strip_inline(&inner));
                    } else {
                        out.push('*');
                        i += 1;
                    }
                } else if i + 1 < n && chars[i + 1] != ' ' && chars[i + 1] != '\t' {
                    let open = i + 1;
                    if let Some(pos) = find_md_marker(&chars, open, &['*']) {
                        let inner: String = chars[open..pos].iter().collect();
                        i = pos + 1;
                        out.push_str(&strip_inline(&inner));
                    } else {
                        out.push('*');
                        i += 1;
                    }
                } else {
                    out.push('*');
                    i += 1;
                }
            }
            '`' => {
                i += 1;
                let start = i;
                while i < n && chars[i] != '`' {
                    i += 1;
                }
                let inner: String = chars[start..i].iter().collect();
                if i < n {
                    i += 1;
                }
                out.push_str(inner.trim());
            }
            // A `_` run wedged between two alphanumerics is literal text,
            // not an emphasis delimiter (CommonMark left/right-flanking rule).
            '_' if underscore_is_intraword(
                &chars,
                i,
                chars[i..].iter().take_while(|c| **c == '_').count(),
            ) =>
            {
                while i < n && chars[i] == '_' {
                    out.push('_');
                    i += 1;
                }
            }
            '_' => {
                if i + 2 < n && chars[i + 1] == '_' && chars[i + 2] == '_' {
                    let open = i + 3;
                    if let Some(pos) = find_md_marker(&chars, open, &['_', '_', '_']) {
                        let inner: String = chars[open..pos].iter().collect();
                        i = pos + 3;
                        out.push_str(&strip_inline(&inner));
                    } else {
                        out.push('_');
                        i += 1;
                    }
                } else if i + 1 < n && chars[i + 1] == '_' {
                    let open = i + 2;
                    if let Some(pos) = find_md_marker(&chars, open, &['_', '_']) {
                        let inner: String = chars[open..pos].iter().collect();
                        i = pos + 2;
                        out.push_str(&strip_inline(&inner));
                    } else {
                        out.push('_');
                        i += 1;
                    }
                } else if i + 1 < n && chars[i + 1] != ' ' && chars[i + 1] != '\t' {
                    let open = i + 1;
                    if let Some(pos) = find_md_marker(&chars, open, &['_']) {
                        let inner: String = chars[open..pos].iter().collect();
                        i = pos + 1;
                        out.push_str(&strip_inline(&inner));
                    } else {
                        out.push('_');
                        i += 1;
                    }
                } else {
                    out.push('_');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

pub fn find_md_marker(chars: &[char], from: usize, marker: &[char]) -> Option<usize> {
    let ml = marker.len();
    if ml == 0 {
        return None;
    }
    let end = chars.len().saturating_sub(ml - 1);
    (from..end).find(|&i| &chars[i..i + ml] == marker)
}

// split_sentences, split_at_sentences, tokenize_keywords, has_keyword_overlap
// — all re-exported from crate::shared above.
