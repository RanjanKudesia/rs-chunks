//! Text classification shared by the PPTX chunking strategies.

use super::slide_model::ContentType;
use super::common::{CLASSIFY_LONG_CHARS, CLASSIFY_SHORT_CHARS};

// ── Text classification ───────────────────────────────────────────────────────

pub fn classify_chunk(text: &str) -> ContentType {
    if text.is_empty() {
        return ContentType::PlainParagraph;
    }
    if text.lines().any(|l| l.starts_with("Table:")) {
        return ContentType::Table;
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if !lines.is_empty()
        && lines
            .iter()
            .all(|l| is_bullet_line(l.trim()) || is_numbered_line(l.trim()))
    {
        return ContentType::BulletNumberedList;
    }
    if text.len() <= 200 && lines.len() <= 4 {
        let joined = lines.join(" ");
        if joined.split_whitespace().count() <= 12 && is_heading_style(&joined) {
            return ContentType::HeadingSection;
        }
    }
    if text.len() > CLASSIFY_LONG_CHARS {
        ContentType::LongSingleParagraph
    } else if text.len() < CLASSIFY_SHORT_CHARS {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

pub fn is_heading_style(text: &str) -> bool {
    let t = text.trim();
    let alpha: String = t.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.is_empty() {
        return false;
    }
    if alpha.len() >= 2 && alpha == alpha.to_uppercase() {
        return true;
    }
    if t.ends_with(':') {
        return true;
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.len() == 1 {
        return words[0]
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            && words[0].len() >= 4;
    }
    if words.len() <= 8 {
        let title_cased = words
            .iter()
            .all(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(true));
        if title_cased && !looks_like_sentence(t) {
            return true;
        }
    }
    false
}

pub fn looks_like_sentence(line: &str) -> bool {
    let words = line.split_whitespace().count();
    words >= 8 || line.ends_with('.') || line.ends_with('!') || line.ends_with('?')
}

pub fn is_bullet_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with('\u{2022}')
        || t.starts_with('\u{25E6}')
        || t.starts_with('\u{25AA}')
        || t.starts_with('\u{25B8}')
}

pub fn is_numbered_line(line: &str) -> bool {
    let t = line.trim();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return false;
    }
    let rest = &t[digits.len()..];
    matches!(rest.chars().next(), Some('.') | Some(')')) && rest.len() > 2
}
