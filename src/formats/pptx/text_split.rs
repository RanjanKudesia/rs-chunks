//! Oversized-text splitting shared by the PPTX chunking strategies.

use crate::shared::split_sentences;

// ── Text splitting ────────────────────────────────────────────────────────────

pub fn split_large_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.trim().to_string()];
    }
    let sentences = split_sentences(text);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for sentence in sentences {
        let candidate = if current.is_empty() {
            sentence.clone()
        } else {
            format!("{current} {sentence}")
        };
        if candidate.len() <= max_chars {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(current.trim().to_string());
            }
            current = sentence;
            while current.len() > max_chars {
                let budget = crate::shared::floor_char_boundary(&current, max_chars);
                let split_at = current[..budget]
                    .rfind(' ')
                    .map(|i| i + 1)
                    .unwrap_or(budget)
                    .max(1);
                let split_at = crate::shared::floor_char_boundary(&current, split_at);
                chunks.push(current[..split_at].trim().to_string());
                current = current[split_at..].trim().to_string();
            }
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() {
        vec![text.trim().to_string()]
    } else {
        chunks.into_iter().filter(|c| !c.is_empty()).collect()
    }
}

// split_sentences — re-exported from crate::shared above.
