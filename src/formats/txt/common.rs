/// Shared types, block parser, and helpers for all TXT chunking strategies.
///
/// TXT has no explicit structure markers, so headings, code blocks, tables,
/// and lists are all detected heuristically from whitespace, capitalisation,
/// and punctuation patterns.

// Re-export shared utilities so strategy files can keep importing from super::common.
pub use crate::shared::{
    has_keyword_overlap, split_at_sentences, split_sentences, tokenize_keywords,
};

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

#[derive(Debug, Clone)]
pub struct TxtBlock {
    pub content: String,
    pub content_type: ContentType,
}

// ── Chunk output ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChunkRecordInput {
    pub content_type: ContentType,
    pub content: String,
    pub metadata: serde_json::Value,
}

// ── Block parser ──────────────────────────────────────────────────────────────

pub fn parse_txt_blocks(text: &str) -> Vec<TxtBlock> {
    split_blocks(text)
        .into_iter()
        .flat_map(|b| {
            let ct = classify_block(&b);
            // Bound the block before it becomes a chunk (#68). A table's rows
            // carry its structure and a code block's lines are its meaning, so
            // both are left whole — `table` is documented as "kept whole".
            // Everything else is prose, and prose is divisible.
            let divisible = !matches!(ct, ContentType::Table | ContentType::CodeBlock);
            let parts = if divisible {
                crate::shared::split_block_on_lines_and_sentences(&b, MAX_CHUNK_CHARS)
            } else {
                vec![b]
            };
            parts
                .into_iter()
                .map(|content| TxtBlock {
                    // Re-classify: a piece of a split block is not necessarily
                    // the same kind as the whole, and its length certainly is
                    // not (`long_single_paragraph` is length-derived).
                    content_type: classify_block(&content),
                    content,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn split_blocks(text: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for raw in text.split("\n\n") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            result.extend(split_at_setext_boundaries(trimmed));
        }
    }
    result
}

pub fn split_at_setext_boundaries(block: &str) -> Vec<String> {
    let lines: Vec<&str> = block.lines().collect();
    if lines.len() < 2 {
        return vec![block.trim().to_string()];
    }
    let mut result: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if i + 1 < lines.len()
            && !lines[i].trim().is_empty()
            && is_setext_underline(lines[i + 1])
        {
            if !current.is_empty() {
                let s = current.join("\n").trim().to_string();
                if !s.is_empty() {
                    result.push(s);
                }
                current.clear();
            }
            result.push(format!("{}\n{}", lines[i].trim(), lines[i + 1].trim()));
            i += 2;
            continue;
        }
        current.push(lines[i]);
        i += 1;
    }
    if !current.is_empty() {
        let s = current.join("\n").trim().to_string();
        if !s.is_empty() {
            result.push(s);
        }
    }
    result
}

pub fn is_setext_underline(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 4
        && !t.contains('|')
        && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-'))
}

// ── Block classification ──────────────────────────────────────────────────────

pub fn classify_block(block: &str) -> ContentType {
    let lines: Vec<&str> = block
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        return ContentType::ShortDisconnectedParagraph;
    }

    if block.trim_start().starts_with("```") {
        return ContentType::CodeBlock;
    }
    if (lines[0] == "{" || lines[0].starts_with("{\"")) && block.trim_end().ends_with('}') {
        return ContentType::CodeBlock;
    }
    if looks_like_command_block(&lines) {
        return ContentType::CodeBlock;
    }

    if lines.len() == 2 && is_setext_underline(lines[1]) {
        return ContentType::HeadingSection;
    }
    if lines.len() == 1
        && (lines[0].starts_with("# ") || lines[0].starts_with("## "))
    {
        return ContentType::HeadingSection;
    }
    {
        let alpha: String = lines[0].chars().filter(|c| c.is_alphabetic()).collect();
        if !alpha.is_empty()
            && alpha == alpha.to_uppercase()
            && lines[0].len() <= 80
            && !lines[0].contains('|')
            && lines.len() == 1
            && !is_all_caps_non_heading(lines[0], &alpha)
        {
            return ContentType::HeadingSection;
        }
    }

    let pipe_count = lines.iter().filter(|l| l.contains('|')).count();
    if lines.len() >= 2 && pipe_count * 2 >= lines.len() {
        return ContentType::Table;
    }

    if is_list_block(&lines) {
        return ContentType::BulletNumberedList;
    }

    if block.len() > 500 {
        ContentType::LongSingleParagraph
    } else if block.len() < 80 {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

/// Lines that are ALL-CAPS but are not section headings (TECH_DEBT #33).
///
/// The rule "a single ALL-CAPS line is a heading" was measured against a
/// three-document corpus and was right 30 times out of 30 — because all three
/// documents happened to use capitals only for section titles. Widening the
/// corpus with real books ([#34](TECH_DEBT.md)) produced counter-examples that
/// nobody constructed, and these are the three shapes they take.
///
/// Deliberately narrow. Each rule was checked against **28 genuine headings**
/// from the corpus — including `WARNING`, `NOTE`, `CAUTION`, `NOTE:` and
/// `ETYMOLOGY.` — and loses none of them. The admonition blocklist the tracker
/// originally proposed is *not* here: none of the real false positives is an
/// admonition, and in `adversarial_allcaps.txt` those words genuinely are
/// headings.
fn is_all_caps_non_heading(line: &str, alpha: &str) -> bool {
    let t = line.trim();

    // A decorative fence is a machine banner, not a title:
    // `*** START OF THE PROJECT GUTENBERG EBOOK … ***`.
    if t.starts_with("***") && t.ends_with("***") {
        return true;
    }

    // Too few letters to be a title — `1.F.` is a licence clause number.
    if alpha.chars().count() < 3 {
        return true;
    }

    // A bare numeral printed above the title it belongs to: `I.`, `II.`, `III.`
    // The trailing period is required, so a word like `MIX` is untouched.
    if t.ends_with('.') {
        let stem = &t[..t.len() - 1];
        if !stem.is_empty() && stem.chars().all(|c| "IVXLCDM".contains(c)) {
            return true;
        }
    }

    // A title does not end mid-clause. `MOBY-DICK;` is a title *continued* on
    // the next line. A trailing colon is NOT included — `NOTE:` and `WARNING:`
    // are ordinary heading punctuation.
    t.ends_with(';') || t.ends_with(',')
}

pub fn is_list_block(lines: &[&str]) -> bool {
    if lines.is_empty() {
        return false;
    }
    let list_count = lines.iter().filter(|l| is_list_item_line(l)).count();
    is_list_item_line(lines[0]) && list_count * 2 >= lines.len()
}

pub fn is_list_item_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    if t.starts_with("[x]") || t.starts_with("[ ]") || t.starts_with("[X]") {
        return true;
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let rest = &t[digits.len()..];
        if matches!(rest.chars().next(), Some('.') | Some(')')) && rest.len() > 1 {
            return true;
        }
    }
    if t.len() >= 3 {
        let mut chars = t.chars();
        let first = chars.next().unwrap_or_default();
        if first.is_ascii_alphabetic() {
            if let Some(sep) = chars.next() {
                if sep == ')' || sep == '.' {
                    return true;
                }
            }
        }
    }
    false
}

pub fn looks_like_command_block(lines: &[&str]) -> bool {
    const COMMAND_STARTS: &[&str] = &[
        "source ", "python ", "python3 ", "cargo ", "npm ", "yarn ", "cd ",
        "git ", "export ", "pip ", "pip3 ", "brew ", "apt ", "sudo ", "bash ",
        "sh ", "make ", "./", "docker ", "kubectl ",
    ];
    lines.iter().any(|l| {
        let t = if l.starts_with('#') {
            l.trim_start_matches('#').trim()
        } else {
            l.trim()
        };
        COMMAND_STARTS.iter().any(|p| t.starts_with(p))
    })
}

// ── Heading helpers ───────────────────────────────────────────────────────────

pub fn extract_heading_text(block: &str) -> String {
    let lines: Vec<&str> = block.lines().collect();
    if lines.is_empty() {
        return block.trim().to_string();
    }
    if lines.len() >= 2 && is_setext_underline(lines[1].trim()) {
        return lines[0].trim().to_string();
    }
    if lines[0].trim().starts_with('#') {
        return lines[0].trim().trim_start_matches('#').trim().to_string();
    }
    lines[0].trim().to_string()
}

/// Infer heading level from block content.
///   `=` underline → 1
///   `-` underline → 2
///   `#` / `##`   → 1 / 2
///   ALL CAPS (no underline) → 2
pub fn heading_level_txt(block: &str) -> u8 {
    let lines: Vec<&str> = block.lines().collect();
    if lines.len() >= 2 {
        let ul = lines[1].trim();
        if ul.len() >= 3 && ul.chars().all(|c| c == '=') {
            return 1;
        }
        if ul.len() >= 3 && ul.chars().all(|c| c == '-') {
            return 2;
        }
    }
    let first = lines[0].trim();
    if first.starts_with("# ") {
        return 1;
    }
    if first.starts_with("## ") {
        return 2;
    }
    if first.starts_with("### ") {
        return 3;
    }
    2
}

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

// ── Prose helpers ─────────────────────────────────────────────────────────────

pub fn classify_prose(text: &str) -> ContentType {
    if text.len() > 900 {
        ContentType::LongSingleParagraph
    } else if text.len() < 90 {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

pub fn txt_metadata(section_heading: Option<String>) -> serde_json::Value {
    serde_json::json!({
        "footnotes_captions": [],
        "page_number": serde_json::Value::Null,
        "section_heading": section_heading,
        "document_metadata": { "source_type": "txt" }
    })
}

// split_sentences, split_at_sentences, tokenize_keywords, has_keyword_overlap
// — all re-exported from crate::shared at the top of this file.

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_setext_underline ───────────────────────────────────────────────────

    #[test]
    fn setext_equals_line_is_underline() {
        assert!(is_setext_underline("===="));
        assert!(is_setext_underline("  ====  "));
    }

    #[test]
    fn setext_dashes_line_is_underline() {
        assert!(is_setext_underline("----"));
    }

    #[test]
    fn setext_too_short_is_not_underline() {
        assert!(!is_setext_underline("==="));
    }

    #[test]
    fn setext_with_pipe_char_is_not_underline() {
        assert!(!is_setext_underline("=|=="));
    }

    #[test]
    fn setext_mixed_chars_is_not_underline() {
        assert!(!is_setext_underline("=-="));
    }

    // ── classify_block ────────────────────────────────────────────────────────

    #[test]
    fn classify_block_backtick_fence_is_code() {
        assert_eq!(classify_block("```\ncode here\n```").as_str(), "code_block");
    }

    #[test]
    fn classify_block_setext_heading() {
        assert_eq!(classify_block("Title Here\n==========").as_str(), "heading");
    }

    #[test]
    fn classify_block_atx_heading() {
        assert_eq!(classify_block("# My Heading").as_str(), "heading");
        assert_eq!(classify_block("## Sub Heading").as_str(), "heading");
    }

    #[test]
    fn classify_block_all_caps_single_line_is_heading() {
        assert_eq!(classify_block("INTRODUCTION").as_str(), "heading");
    }

    #[test]
    fn classify_block_bullet_list() {
        let b = "- item one\n- item two\n- item three";
        assert_eq!(classify_block(b).as_str(), "bullet_list");
    }

    #[test]
    fn classify_block_numbered_list() {
        let b = "1. First item\n2. Second item\n3. Third item";
        assert_eq!(classify_block(b).as_str(), "bullet_list");
    }

    #[test]
    fn classify_block_table_by_pipe_ratio() {
        let b = "Col A | Col B\nVal 1 | Val 2\nVal 3 | Val 4";
        assert_eq!(classify_block(b).as_str(), "table");
    }

    #[test]
    fn classify_block_long_text_over_500() {
        let b = "word ".repeat(110); // > 500 chars
        assert_eq!(classify_block(&b).as_str(), "long_single_paragraph");
    }

    #[test]
    fn classify_block_short_under_80() {
        assert_eq!(classify_block("Short text here.").as_str(), "short_disconnected_paragraph");
    }

    #[test]
    fn classify_block_json_object_is_code() {
        let b = "{\"key\": \"value\"}";
        assert_eq!(classify_block(b).as_str(), "code_block");
    }

    // ── is_list_item_line ─────────────────────────────────────────────────────

    #[test]
    fn list_item_dash_prefix() {
        assert!(is_list_item_line("- item text"));
    }

    #[test]
    fn list_item_star_prefix() {
        assert!(is_list_item_line("* item text"));
    }

    #[test]
    fn list_item_plus_prefix() {
        assert!(is_list_item_line("+ item text"));
    }

    #[test]
    fn list_item_numbered_dot() {
        assert!(is_list_item_line("1. First item"));
        assert!(is_list_item_line("10. Tenth item"));
    }

    #[test]
    fn list_item_numbered_paren() {
        assert!(is_list_item_line("1) First item"));
    }

    #[test]
    fn list_item_checkbox_variants() {
        assert!(is_list_item_line("[x] done task"));
        assert!(is_list_item_line("[ ] pending task"));
        assert!(is_list_item_line("[X] also done"));
    }

    #[test]
    fn list_item_plain_sentence_is_not_list() {
        assert!(!is_list_item_line("just a plain sentence here"));
    }

    // ── heading helpers ───────────────────────────────────────────────────────

    #[test]
    fn extract_heading_text_from_setext() {
        assert_eq!(extract_heading_text("My Title\n========"), "My Title");
    }

    #[test]
    fn extract_heading_text_from_atx() {
        assert_eq!(extract_heading_text("## Section Name"), "Section Name");
        assert_eq!(extract_heading_text("# Top Level"), "Top Level");
    }

    #[test]
    fn heading_level_setext_equals_is_1() {
        assert_eq!(heading_level_txt("Title\n====="), 1);
    }

    #[test]
    fn heading_level_setext_dashes_is_2() {
        assert_eq!(heading_level_txt("Title\n-----"), 2);
    }

    #[test]
    fn heading_level_atx_h1_and_h2() {
        assert_eq!(heading_level_txt("# Heading"), 1);
        assert_eq!(heading_level_txt("## Heading"), 2);
        assert_eq!(heading_level_txt("### Heading"), 3);
    }

    // ── heading stack helpers ─────────────────────────────────────────────────

    #[test]
    fn heading_stack_pops_deeper_levels_on_update() {
        let mut stack: Vec<(u8, String)> = Vec::new();
        update_heading_stack(&mut stack, 1, "Chapter".to_string());
        update_heading_stack(&mut stack, 2, "Section".to_string());
        assert_eq!(stack.len(), 2);
        update_heading_stack(&mut stack, 1, "New Chapter".to_string());
        assert_eq!(stack.len(), 1);
        assert_eq!(current_section_heading(&stack), Some("New Chapter".to_string()));
    }

    #[test]
    fn heading_path_strings_returns_all_ancestors() {
        let mut stack: Vec<(u8, String)> = Vec::new();
        update_heading_stack(&mut stack, 1, "Chapter".to_string());
        update_heading_stack(&mut stack, 2, "Section".to_string());
        let path = heading_path_strings(&stack);
        assert_eq!(path, vec!["Chapter", "Section"]);
    }

    #[test]
    fn current_section_level_returns_innermost() {
        let mut stack: Vec<(u8, String)> = Vec::new();
        update_heading_stack(&mut stack, 1, "H1".to_string());
        update_heading_stack(&mut stack, 3, "H3".to_string());
        assert_eq!(current_section_level(&stack), 3);
    }

    // ── classify_prose ────────────────────────────────────────────────────────

    #[test]
    fn classify_prose_over_900_is_long() {
        let t = "x".repeat(1000);
        assert_eq!(classify_prose(&t).as_str(), "long_single_paragraph");
    }

    #[test]
    fn classify_prose_under_90_is_short() {
        assert_eq!(classify_prose("short").as_str(), "short_disconnected_paragraph");
    }

    #[test]
    fn classify_prose_medium_is_plain() {
        let t = "x".repeat(200);
        assert_eq!(classify_prose(&t).as_str(), "plain_paragraph");
    }
}

#[cfg(test)]
mod all_caps_tests {
    use super::*;

    fn is_heading(line: &str) -> bool {
        classify_block(line) == ContentType::HeadingSection
    }

    /// The false positives real books produced once the corpus was widened
    /// (TECH_DEBT #33, #34). None of these was constructed.
    #[test]
    fn machine_markers_and_numbering_are_not_headings() {
        for line in [
            "*** START OF THE PROJECT GUTENBERG EBOOK MOBY DICK; OR, THE WHALE ***",
            "*** END OF THE PROJECT GUTENBERG EBOOK MOBY DICK; OR, THE WHALE ***",
            "1.F.",
            "MOBY-DICK;",
            "I.",
            "II.",
            "III.",
        ] {
            assert!(!is_heading(line), "{line:?} must not be a heading");
        }
    }

    /// Every genuine heading the corpus contains must survive the narrowing.
    /// The admonition blocklist the tracker proposed would have failed this.
    #[test]
    fn genuine_all_caps_headings_survive() {
        for line in [
            "THE ECONOMICS OF OPEN SOURCE SOFTWARE",
            "EXECUTIVE SUMMARY",
            "A SCANDAL IN BOHEMIA",
            "ETYMOLOGY.",
            "EXTRACTS.",
            "CONTENTS",
            "INTRODUCTION",
            "WARNING",
            "NOTE",
            "CAUTION",
            "APPENDIX A: GLOSSARY",
            "KEY METRICS (PSEUDO TABLE)",
            "DATES, MONEY, VERSIONS",
            "THE FULL PROJECT GUTENBERG™ LICENSE",
        ] {
            assert!(is_heading(line), "{line:?} must stay a heading");
        }
    }

    /// A trailing colon is ordinary heading punctuation, unlike a semicolon or
    /// comma — which mark a line continued onto the next.
    #[test]
    fn a_trailing_colon_is_not_a_continuation() {
        assert!(is_heading("NOTE:"));
        assert!(is_heading("WARNING:"));
        assert!(!is_heading("MOBY-DICK;"));
        assert!(!is_heading("DATES, MONEY,"));
    }

    /// The Roman-numeral rule requires the trailing period, so an ordinary word
    /// made only of those letters is untouched.
    #[test]
    fn a_word_of_roman_letters_is_still_a_heading() {
        assert!(is_heading("MIX"));
        assert!(is_heading("CIVIL"));
        assert!(!is_heading("XVI."));
    }

    /// Honest limit: an acronym string is not lexically separable from a title,
    /// and this rule does not pretend otherwise (#33).
    #[test]
    fn an_acronym_line_is_still_read_as_a_heading() {
        assert!(is_heading("NASA JPL USA"));
    }
}
