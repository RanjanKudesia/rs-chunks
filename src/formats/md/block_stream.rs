//! Markdown block parsing that can be fed a document in pieces.
//!
//! [`common::parse_markdown_blocks`] takes a whole document and returns every
//! block. That is one of the three things stopping any pipeline format from
//! emitting a chunk before it has read its whole input ([#87](TECH_DEBT.md)).
//!
//! ## Why a blank line is a safe place to resume
//!
//! The block parser has **no construct that survives a blank line** outside a
//! fenced code block: an empty line calls `flush_text_blocks`, which closes the
//! open paragraph, list *and* table. A list broken by a blank line already
//! becomes two `List` blocks today, which is the same statement from the other
//! side. So text before such a line parses to exactly the blocks the whole
//! document would produce there, whatever follows.
//!
//! The one lookahead in the parser — a setext underline on the *next* line —
//! cannot cross the cut either, because the line after the cut point is the
//! blank line itself and a blank line is not an underline.
//!
//! That makes the rule here exact rather than heuristic, and
//! `feeding_a_document_in_pieces_matches_parsing_it_whole` checks it against the
//! corpus at every possible split rather than taking the argument on trust.

use super::common::{parse_blocks_from, MdBlock};

/// Accumulates markdown text and yields blocks as they become complete.
#[derive(Default)]
pub(crate) struct BlockStream {
    /// Text seen but not yet provably complete.
    pending: String,
    /// Index the next emitted block will carry.
    next_index: usize,
}

impl BlockStream {
    pub fn new() -> BlockStream {
        BlockStream::default()
    }

    /// Append text and return every block that is now complete.
    pub fn push(&mut self, text: &str) -> Vec<MdBlock> {
        self.pending.push_str(text);
        let cut = resume_point(&self.pending);
        if cut == 0 {
            return Vec::new();
        }
        let rest = self.pending.split_off(cut);
        let ready = std::mem::replace(&mut self.pending, rest);
        self.emit(&ready)
    }

    /// Yield whatever is left. The tail has no blank line after it, so nothing
    /// else can be waiting on one.
    pub fn finish(&mut self) -> Vec<MdBlock> {
        let ready = std::mem::take(&mut self.pending);
        self.emit(&ready)
    }

    fn emit(&mut self, text: &str) -> Vec<MdBlock> {
        if text.trim().is_empty() {
            return Vec::new();
        }
        let blocks = parse_blocks_from(text, self.next_index);
        self.next_index += blocks.len();
        blocks
    }
}

/// Byte offset just past the last blank line that is not inside a code fence,
/// or 0 if there is none.
pub(crate) fn resume_point(text: &str) -> usize {
    let mut in_fence = false;
    let (mut cut, mut offset) = (0usize, 0usize);
    for line in text.split_inclusive('\n') {
        let compact = line.trim();
        if compact.starts_with("```") {
            in_fence = !in_fence;
        } else if compact.is_empty() && !in_fence && line.ends_with('\n') {
            cut = offset + line.len();
        }
        offset += line.len();
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::md::common::parse_markdown_blocks;

    fn whole(text: &str) -> Vec<(String, usize)> {
        parse_markdown_blocks(text).into_iter().map(|b| (b.content, b.index)).collect()
    }

    fn in_pieces(text: &str, at: &[usize]) -> Vec<(String, usize)> {
        let mut stream = BlockStream::new();
        let mut out = Vec::new();
        let mut last = 0;
        for cut in at.iter().copied().chain(std::iter::once(text.len())) {
            // Only split on a char boundary; the corpus has multi-byte text.
            let cut = cut.min(text.len());
            if !text.is_char_boundary(cut) || cut < last {
                continue;
            }
            out.extend(stream.push(&text[last..cut]));
            last = cut;
        }
        out.extend(stream.push(&text[last..]));
        out.extend(stream.finish());
        out.into_iter().map(|b| (b.content, b.index)).collect()
    }

    /// The whole claim, checked at **every** byte offset of each document rather
    /// than at a few chosen ones — a split is only interesting if it lands
    /// somewhere awkward, and picking the splits by hand is how you miss those.
    #[test]
    fn feeding_a_document_in_pieces_matches_parsing_it_whole() {
        let documents = [
            "# Title\n\nA paragraph that runs on.\n\nAnother one.\n",
            "Setext\n======\n\nbody text here\n",
            "- one\n- two\n\n- three\n\ntrailing prose\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n\nafter the table\n",
            // A fence containing blank lines and a line that looks like a rule.
            "intro\n\n```rust\nfn main() {\n\n    // ---\n}\n```\n\nafter\n",
            // No trailing newline, and a document that is one long paragraph.
            "just one paragraph with no newline at the end",
            "# A\n## B\n### C\n\ntext\n\n---\n\nmore\n",
        ];
        for document in documents {
            let expected = whole(document);
            for cut in 0..=document.len() {
                assert_eq!(in_pieces(document, &[cut]), expected, "split at {cut} of {document:?}");
            }
            // And fed one byte at a time, the worst case for a buffering parser.
            let every: Vec<usize> = (0..document.len()).collect();
            assert_eq!(in_pieces(document, &every), expected, "byte-at-a-time {document:?}");
        }
    }

    #[test]
    fn a_fence_is_never_cut_even_though_it_contains_blank_lines() {
        let text = "```\n\n\n```\n\nafter\n";
        // The blank lines inside the fence must not become resume points; the
        // only one that counts is the blank line after the closing fence.
        assert_eq!(resume_point(text), text.find("after").unwrap());
    }

    #[test]
    fn an_incomplete_final_line_is_never_treated_as_a_boundary() {
        // Text still arriving: the trailing "" is not a blank *line* yet.
        assert_eq!(resume_point("a\n\nb"), 3);
        assert_eq!(resume_point("a\nb"), 0);
    }
}
