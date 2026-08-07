//! Content-type / chunk-record / slide content model, plus chunk metadata.

use serde_json::{json, Value};

// ── Content types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    PlainParagraph,
    HeadingSection,
    BulletNumberedList,
    Table,
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

// ── Chunk output ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChunkRecordInput {
    pub content_type: ContentType,
    pub content: String,
    pub metadata: Value,
}

// ── Slide content model ───────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct SlideContent {
    pub title: Option<String>,
    pub body_paragraphs: Vec<String>,
    pub has_table: bool,
    /// Speaker notes extracted from the slide's associated notes slide XML.
    pub notes_text: Option<String>,
    /// `r:dm` relationship ids of any SmartArt diagrams on the slide. The text
    /// lives in a separate `ppt/diagrams/dataN.xml` part, so the parser can only
    /// record the ids here; `read_all_slides` resolves and reads them.
    pub diagram_rids: Vec<String>,
    /// `r:id` values of any embedded charts on the slide. Like SmartArt, the
    /// plotted numbers live in a separate `ppt/charts/chartN.xml` part.
    pub chart_rids: Vec<String>,
}

impl SlideContent {
    pub fn is_empty_body(&self) -> bool {
        self.body_paragraphs.iter().all(|p| p.trim().is_empty())
    }

    /// True for slides that look like section dividers (title only, no body).
    pub fn is_section_divider(&self) -> bool {
        self.title.is_some() && self.is_empty_body()
    }

    pub fn all_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref t) = self.title {
            parts.push(t.clone());
        }
        for p in &self.body_paragraphs {
            let trimmed = p.trim().to_string();
            if !trimmed.is_empty() {
                parts.push(trimmed);
            }
        }
        if self.has_table && !parts.is_empty() {
            // Prefix the first body paragraph (not the title) with "Table:" so
            // classify_chunk can detect tables even when a title is present.
            let mark_idx = if self.title.is_some() && parts.len() > 1 {
                1
            } else {
                0
            };
            parts[mark_idx] = format!("Table: {}", parts[mark_idx]);
        }
        let main = parts.join("\n");
        // Append speaker notes with a separator so all chunking modes benefit.
        if let Some(ref notes) = self.notes_text {
            let n = notes.trim();
            if !n.is_empty() {
                return format!("{main}\n\n[Notes]\n{n}");
            }
        }
        main
    }
}

// ── Metadata ──────────────────────────────────────────────────────────────────

pub fn pptx_metadata(
    slide_number: usize,
    slide_title: Option<String>,
    section_heading: Option<String>,
    total_slides: usize,
) -> Value {
    json!({
        "slide_number":     slide_number,
        "slide_range":      [slide_number, slide_number],
        "slide_title":      slide_title,
        "section_heading":  section_heading,
        "document_metadata": {
            "source_type":  "pptx",
            "total_slides": total_slides,
        }
    })
}
