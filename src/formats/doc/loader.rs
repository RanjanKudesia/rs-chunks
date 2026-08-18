//! Turning a `.doc` file's bytes into the paragraph stream every mode chunks.
//!
//! A `.doc`'s CP space runs main text, then footnotes, headers/footers,
//! annotations, endnotes and text boxes, each with its own length in the FIB
//! ([MS-DOC] 2.5.4). Each is reconstructed separately, because paragraph
//! properties are keyed by FC and only resolve against the pieces of the story
//! they belong to.

use super::fib::Fib;
use super::paragraph_props::{index_by_paragraph, ParagraphProp};
use super::piece_table::{self, Piece, ReconstructedText};
use super::stylesheet::StyleSheet;
use super::text_extractor::{self, DocParagraph, ParagraphType};
use super::{cfb_reader, fib, stylesheet};

/// One story's reconstructed text alongside the properties of its paragraphs.
pub(super) struct Story {
    pub text: ReconstructedText,
    pub props: Vec<Option<ParagraphProp>>,
}

/// Everything parsed out of the file once, so the callers that need more than
/// the paragraph list (images, markdown) do not re-parse.
pub(super) struct ParsedDoc {
    pub word_doc: Vec<u8>,
    pub table: Vec<u8>,
    pub fib: Fib,
    pub stylesheet: StyleSheet,
    pub props: Vec<ParagraphProp>,
}

impl ParsedDoc {
    pub fn open(bytes: &[u8]) -> Result<Self, String> {
        let mut cfb = cfb_reader::DocCfb::open(bytes)?;
        let word_doc = cfb.word_document_stream()?;
        let fib = fib::parse_fib(&word_doc)?;
        let table = cfb.table_stream(fib.f_which_tbl_stm)?;
        let stylesheet = stylesheet::parse_stylesheet(&table, fib.fc_stshf, fib.lcb_stshf)?;
        let props = super::paragraph_props::parse_paragraph_props(
            &word_doc,
            &table,
            fib.fc_plcf_papx,
            fib.lcb_plcf_papx,
        )?;
        Ok(ParsedDoc {
            word_doc,
            table,
            fib,
            stylesheet,
            props,
        })
    }

    /// Reconstruct the CP range `[cp_from, cp_to)` and resolve its paragraph
    /// properties against it.
    fn story(&self, cp_from: u32, cp_to: i32) -> Result<(Story, Vec<Piece>), String> {
        let pieces = piece_table::parse_pieces_range(
            &self.table,
            self.fib.fc_clx,
            self.fib.lcb_clx,
            cp_from,
            cp_to,
        )?;
        let text = piece_table::reconstruct_from_pieces(&self.word_doc, &pieces);
        let props = index_by_paragraph(&self.props, &pieces, &text);
        Ok((Story { text, props }, pieces))
    }

    pub fn main_story(&self) -> Result<(Story, Vec<Piece>), String> {
        self.story(0, self.fib.ccp_text)
    }

    /// Paragraphs of the main text, each tagged with its Word-paragraph
    /// ordinal so inline images can be anchored back to it.
    pub fn main_paragraphs_indexed(&self) -> Result<Vec<(usize, DocParagraph)>, String> {
        let (story, _) = self.main_story()?;
        Ok(text_extractor::extract_paragraphs_indexed(
            &story.text,
            &story.props,
            &self.stylesheet,
        ))
    }

    /// The full paragraph stream: main text followed by the labelled stories
    /// that come after it.
    pub fn all_paragraphs(&self) -> Result<Vec<DocParagraph>, String> {
        let mut out: Vec<DocParagraph> = self
            .main_paragraphs_indexed()?
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        self.append_side_stories(&mut out);
        Ok(out)
    }

    /// Append the stories that follow the main text — footnotes, headers and
    /// footers, annotations, endnotes and text boxes — as labelled paragraphs.
    ///
    /// They are appended rather than interleaved because the binary format
    /// gives no reliable way to place a footnote back at its reference point,
    /// and a labelled block at the end is honest about that (#70).
    ///
    /// They go through the same extractor as the main text, so a table in a
    /// text box — which is where sample.doc keeps its only table — comes out as
    /// a table rather than as cell text run together (#12).
    fn append_side_stories(&self, paragraphs: &mut Vec<DocParagraph>) {
        let f = &self.fib;
        let mut cp = f.ccp_text.max(0) as u32;
        let stories: [(&str, i32); 6] = [
            ("Footnotes", f.ccp_ftn),
            ("Headers and footers", f.ccp_hdd),
            ("Macros", f.ccp_mcr),
            ("Comments", f.ccp_atn),
            ("Endnotes", f.ccp_edn),
            ("Text boxes", f.ccp_txbx),
        ];
        for (label, len) in stories {
            let len = len.max(0) as u32;
            if len == 0 {
                continue;
            }
            let (from, to) = (cp, cp + len);
            cp = to;
            let Ok((story, _)) = self.story(from, to as i32) else {
                continue;
            };
            let extracted =
                text_extractor::extract_paragraphs(&story.text, &story.props, &self.stylesheet);
            let has_content = extracted.iter().any(|p| !p.content.trim().is_empty());
            if !has_content {
                continue;
            }
            // Level 1, so the label closes the main text's heading trail
            // instead of nesting under whatever section it happened to end on
            // — a footnote block is not a subsection of the Conclusion (#12).
            paragraphs.push(DocParagraph::plain(
                format!("[{label}]"),
                ParagraphType::Heading(1),
                None,
            ));
            paragraphs.extend(
                extracted
                    .into_iter()
                    .filter(|p| !p.content.trim().is_empty()),
            );
        }
    }
}
