//! Where a chunk sits in a `.doc`/`.ppt`'s structure.
//!
//! Page provenance ([#11](TECH_DEBT.md), [#18](TECH_DEBT.md)) established the
//! rule the rest of the structural metadata follows: a chunk belongs to the
//! position of the paragraph it *starts* on, because that is what a reader
//! looking it up would turn to. Section breadcrumbs, list depth and table shape
//! are all resolved the same way (#12).

use super::tables::TableShape;
use super::text_extractor::{DocParagraph, ParagraphType};

/// The structural position of one chunk.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ChunkContext {
    /// 1-based page (`.doc`) or slide (`.ppt`), or `None` when the source
    /// declares no such structure.
    pub page_number: Option<usize>,
    /// The nearest enclosing heading.
    pub section_heading: Option<String>,
    /// Level of `section_heading`.
    pub section_heading_level: Option<u8>,
    /// Every enclosing heading, outermost first.
    pub heading_path: Vec<String>,
    /// 0-based list nesting depth, when the chunk starts on a list item.
    pub list_level: Option<u8>,
    /// Shape of the table, when the chunk starts on one.
    pub table: Option<TableShape>,
}

impl ChunkContext {
    /// The breadcrumb trail as one string, matching DOCX's `heading_path`.
    /// `None` rather than an empty string when there is no enclosing heading,
    /// so callers can tell "no section" from "a section named nothing".
    pub fn heading_path_string(&self) -> Option<String> {
        (!self.heading_path.is_empty()).then(|| self.heading_path.join(" > "))
    }
}

/// Maintains the heading stack while walking a paragraph list in reading order.
#[derive(Default)]
struct SectionTracker {
    stack: Vec<(u8, String)>,
}

impl SectionTracker {
    /// Note a heading as it is reached. Called *before* the paragraph's own
    /// context is taken, so a heading names itself rather than its parent —
    /// which is what DOCX's `section` mode does.
    fn observe(&mut self, p: &DocParagraph) {
        let ParagraphType::Heading(level) = p.paragraph_type else {
            return;
        };
        let content = p.content.trim();
        if content.is_empty() {
            return;
        }
        while self.stack.last().map(|(l, _)| *l >= level).unwrap_or(false) {
            self.stack.pop();
        }
        self.stack.push((level, content.to_string()));
    }

    fn context_for(&self, p: &DocParagraph) -> ChunkContext {
        ChunkContext {
            page_number: p.page_index.map(|i| i + 1),
            section_heading: self.stack.last().map(|(_, h)| h.clone()),
            section_heading_level: self.stack.last().map(|(l, _)| *l),
            heading_path: self.stack.iter().map(|(_, h)| h.clone()).collect(),
            list_level: p.list_level,
            table: p.table,
        }
    }
}

/// One paragraph paired with the structural position it occupies.
#[derive(Debug, Clone)]
pub(crate) struct Positioned {
    pub paragraph: DocParagraph,
    pub context: ChunkContext,
}

/// Walk the paragraph list once, resolving each paragraph's position.
pub(crate) fn position(paragraphs: Vec<DocParagraph>) -> Vec<Positioned> {
    let mut tracker = SectionTracker::default();
    paragraphs
        .into_iter()
        .map(|paragraph| {
            tracker.observe(&paragraph);
            let context = tracker.context_for(&paragraph);
            Positioned { paragraph, context }
        })
        .collect()
}

/// The context of the first item in a window — the rule page provenance set.
pub(crate) fn context_of(window: &[Positioned]) -> ChunkContext {
    window.first().map(|p| p.context.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heading(level: u8, text: &str) -> DocParagraph {
        DocParagraph::plain(text.to_string(), ParagraphType::Heading(level), None)
    }

    fn body(text: &str) -> DocParagraph {
        DocParagraph::plain(text.to_string(), ParagraphType::Normal, None)
    }

    #[test]
    fn a_nested_heading_extends_the_breadcrumb() {
        let positioned = position(vec![
            heading(1, "Procedure"),
            heading(2, "Review Stage"),
            body("Instructions about final paper submissions."),
        ]);
        assert_eq!(
            positioned[2].context.heading_path_string().as_deref(),
            Some("Procedure > Review Stage")
        );
        assert_eq!(
            positioned[2].context.section_heading.as_deref(),
            Some("Review Stage")
        );
        assert_eq!(positioned[2].context.section_heading_level, Some(2));
    }

    /// A heading names itself, not the section that contains it.
    #[test]
    fn a_heading_is_its_own_section() {
        let positioned = position(vec![heading(1, "Procedure"), heading(2, "Review Stage")]);
        assert_eq!(
            positioned[1].context.heading_path,
            vec!["Procedure", "Review Stage"]
        );
    }

    /// A same-or-shallower heading closes the deeper ones — otherwise the trail
    /// grows without bound and every later chunk claims sections it left.
    #[test]
    fn a_sibling_heading_pops_the_deeper_ones() {
        let positioned = position(vec![
            heading(1, "First"),
            heading(2, "Sub"),
            heading(3, "Subsub"),
            heading(1, "Second"),
            body("text"),
        ]);
        assert_eq!(positioned[4].context.heading_path, vec!["Second"]);
    }

    #[test]
    fn a_document_without_headings_has_no_breadcrumb() {
        let positioned = position(vec![body("just text")]);
        assert_eq!(positioned[0].context.heading_path_string(), None);
        assert_eq!(positioned[0].context.section_heading, None);
    }
}
