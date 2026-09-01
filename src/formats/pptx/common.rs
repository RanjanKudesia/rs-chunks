//! Shared types, slide parser, and helpers for all PPTX chunking strategies.

/// Every PresentationML OOXML extension routed through the pptx chunker. All
/// store slides under `ppt/slides/`, which the parser enumerates by part name
/// regardless of extension. NOTE: legacy binary `.ppt` is NOT here — it routes
/// to the separate `ppt` chunker.
pub const PPTX_OOXML_EXTS: &[&str] = &[".pptx", ".potx", ".potm", ".ppsx", ".ppsm"];

/// True if `path` ends in any supported PresentationML OOXML extension.
pub fn is_pptx_ooxml(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    PPTX_OOXML_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Human-readable extension list for error messages.
pub fn pptx_exts_display() -> String {
    PPTX_OOXML_EXTS.join(", ")
}

// Re-export shared utilities so strategy files can keep importing from super::common.
pub use crate::shared::{has_keyword_overlap, split_sentences, tokenize_keywords};

pub const MAX_CHUNK_CHARS: usize = 1200;
pub const MIN_CHUNK_CHARS: usize = 350;

/// Threshold above which a single-paragraph text is classified as LongSingleParagraph.
pub const CLASSIFY_LONG_CHARS: usize = 900;
/// Threshold below which a text is classified as ShortDisconnectedParagraph.
pub const CLASSIFY_SHORT_CHARS: usize = 90;

// The implementations live in focused sibling modules; re-export them all so
// every strategy file keeps importing from `super::common` unchanged.
pub use super::archive::*;
pub use super::classify::*;
pub use super::presentation::*;
pub use super::slide_images::*;
pub use super::slide_model::*;
pub use super::slide_xml::*;
pub use super::text_split::*;
pub use super::xml_util::*;

// tokenize_keywords, has_keyword_overlap — re-exported from crate::shared above.

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    // ── ZIP / slide helpers ───────────────────────────────────────────────────

    fn slide_xml(title: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
        <p:txBody>
          <a:p><a:r><a:t>{title}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr>
        <p:txBody>
          <a:p><a:r><a:t>{body}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#
        )
    }

    /// Build a minimal in-memory PPTX ZIP from (title, body) pairs.
    fn make_pptx(slides: &[(&str, &str)]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (i, (title, body)) in slides.iter().enumerate() {
            let path = format!("ppt/slides/slide{}.xml", i + 1);
            zip.start_file(path, opts).unwrap();
            zip.write_all(slide_xml(title, body).as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    // ── parse_slide_xml ───────────────────────────────────────────────────────

    #[test]
    fn parse_slide_xml_extracts_title() {
        let xml = slide_xml("My Title", "Body text here.");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert_eq!(slide.title.as_deref(), Some("My Title"));
    }

    #[test]
    fn parse_slide_xml_extracts_body() {
        let xml = slide_xml("Title", "Body paragraph content.");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert!(!slide.body_paragraphs.is_empty());
        assert!(slide.body_paragraphs[0].contains("Body paragraph"));
    }

    #[test]
    fn parse_slide_xml_empty_body_is_section_divider() {
        let xml = slide_xml("Section Title", "");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert!(
            slide.is_section_divider(),
            "title-only slide should be a section divider"
        );
    }

    #[test]
    fn parse_slide_xml_with_body_is_not_section_divider() {
        let xml = slide_xml("Title", "Body content present.");
        let slide = parse_slide_xml(xml.as_bytes()).unwrap();
        assert!(!slide.is_section_divider());
    }

    #[test]
    fn parse_slide_xml_invalid_xml_returns_error() {
        let bad = b"not xml at all <<>>";
        // Parser is lenient; just check it doesn't panic — any result is fine.
        let _ = parse_slide_xml(bad);
    }

    // ── SlideContent::all_text ────────────────────────────────────────────────

    #[test]
    fn all_text_joins_title_and_body() {
        let s = SlideContent {
            title: Some("Title".to_string()),
            body_paragraphs: vec!["Body".to_string()],
            has_table: false,
            notes_text: None,
            ..Default::default()
        };
        let t = s.all_text();
        assert!(t.contains("Title"));
        assert!(t.contains("Body"));
    }

    #[test]
    fn all_text_appends_notes_with_separator() {
        let s = SlideContent {
            title: Some("Title".to_string()),
            body_paragraphs: vec!["Body".to_string()],
            has_table: false,
            notes_text: Some("Speaker note text.".to_string()),
            ..Default::default()
        };
        let t = s.all_text();
        assert!(t.contains("[Notes]"));
        assert!(t.contains("Speaker note text."));
    }

    #[test]
    fn all_text_table_prefixes_body_paragraph() {
        let s = SlideContent {
            title: Some("Title".to_string()),
            body_paragraphs: vec!["Cell A | Cell B".to_string()],
            has_table: true,
            notes_text: None,
            ..Default::default()
        };
        let t = s.all_text();
        assert!(
            t.contains("Table:"),
            "has_table should prefix body with 'Table:'"
        );
    }

    // ── open_pptx / collect_slide_names ──────────────────────────────────────

    #[test]
    fn open_pptx_with_valid_zip_succeeds() {
        let bytes = make_pptx(&[("Title", "Body")]);
        assert!(open_pptx(&bytes).is_ok());
    }

    #[test]
    fn open_pptx_with_invalid_bytes_returns_error() {
        assert!(open_pptx(b"not a zip file").is_err());
    }

    #[test]
    fn collect_slide_names_finds_slides_in_order() {
        let bytes = make_pptx(&[
            ("Slide 1", "Body 1"),
            ("Slide 2", "Body 2"),
            ("Slide 3", "Body 3"),
        ]);
        let archive = open_pptx(&bytes).unwrap();
        let names = collect_slide_names(&archive);
        assert_eq!(names.len(), 3);
        assert_eq!(names[0].0, 1);
        assert_eq!(names[1].0, 2);
        assert_eq!(names[2].0, 3);
    }

    #[test]
    fn collect_slide_names_excludes_layouts_and_masters() {
        // A slide named ppt/slides/slideLayout1.xml should be filtered out.
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("ppt/slides/slideLayout1.xml", opts).unwrap();
        zip.write_all(b"<layout/>").unwrap();
        zip.start_file("ppt/slides/slide1.xml", opts).unwrap();
        zip.write_all(slide_xml("Title", "Body").as_bytes())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();

        let archive = open_pptx(&bytes).unwrap();
        let names = collect_slide_names(&archive);
        assert_eq!(
            names.len(),
            1,
            "layout should be excluded, only slide1 present"
        );
    }

    // ── classify_chunk ────────────────────────────────────────────────────────

    #[test]
    fn classify_chunk_bullet_lines_is_list() {
        let text = "- First bullet\n- Second bullet\n- Third bullet";
        assert_eq!(classify_chunk(text).as_str(), "bullet_list");
    }

    #[test]
    fn classify_chunk_table_prefix_is_table() {
        let text = "Table: Cell A | Cell B\nRow 2 | Value 2";
        assert_eq!(classify_chunk(text).as_str(), "table");
    }

    #[test]
    fn classify_chunk_long_text_is_long_paragraph() {
        let text = "x".repeat(CLASSIFY_LONG_CHARS + 1);
        assert_eq!(
            classify_chunk(text.as_str()).as_str(),
            "long_single_paragraph"
        );
    }

    #[test]
    fn classify_chunk_short_text_is_short_paragraph() {
        let text = "x".repeat(CLASSIFY_SHORT_CHARS - 1);
        assert_eq!(
            classify_chunk(text.as_str()).as_str(),
            "short_disconnected_paragraph"
        );
    }

    // ── is_heading_style / looks_like_sentence ────────────────────────────────

    #[test]
    fn is_heading_style_all_caps() {
        assert!(is_heading_style("INTRODUCTION"));
    }

    #[test]
    fn is_heading_style_ends_with_colon() {
        assert!(is_heading_style("Summary:"));
    }

    #[test]
    fn looks_like_sentence_long_text() {
        // ≥8 words → sentence
        assert!(looks_like_sentence(
            "this is a sentence with eight words here"
        ));
    }

    #[test]
    fn looks_like_sentence_ends_with_period() {
        assert!(looks_like_sentence("Short text."));
    }

    // ── is_bullet_line / is_numbered_line ─────────────────────────────────────

    #[test]
    fn is_bullet_line_dash_and_star() {
        assert!(is_bullet_line("- item"));
        assert!(is_bullet_line("* item"));
    }

    #[test]
    fn is_bullet_line_unicode_bullets() {
        assert!(is_bullet_line("\u{2022} item"));
    }

    #[test]
    fn is_numbered_line_dot_separator() {
        assert!(is_numbered_line("1. item"));
        assert!(is_numbered_line("10. item"));
    }

    #[test]
    fn is_numbered_line_paren_separator() {
        assert!(is_numbered_line("1) item"));
    }

    #[test]
    fn is_numbered_line_too_many_digits_is_false() {
        assert!(!is_numbered_line("1234. item"));
    }

    // ── split_large_text ──────────────────────────────────────────────────────

    #[test]
    fn split_large_text_short_input_unchanged() {
        let t = "Short text.";
        let parts = split_large_text(t, 1000);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], t);
    }

    #[test]
    fn split_large_text_splits_long_input() {
        let t = "Sentence one. ".repeat(100);
        let parts = split_large_text(&t, 200);
        assert!(parts.len() > 1, "long text should be split");
        assert!(parts.iter().all(|p| !p.is_empty()));
    }
}
