//! Text splitting used by the DOCX structural/default chunk builders.

pub(super) fn semantic_chunks(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.trim().to_string()];
    }

    let sentences = split_sentences(text);
    let mut out = Vec::new();
    let mut current = String::new();

    for s in sentences {
        let candidate = if current.is_empty() {
            s.clone()
        } else {
            format!("{} {}", current, s)
        };

        if candidate.len() <= max_chars {
            current = candidate;
        } else {
            if !current.is_empty() {
                out.push(current.trim().to_string());
            }
            current = s;
        }
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    if out.is_empty() {
        vec![text.trim().to_string()]
    } else {
        out
    }
}

pub(super) fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
            current.clear();
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }

    out
}

pub(super) fn recursive_char_chunks(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }

    let split_at = crate::shared::floor_char_boundary(text, max_chars.min(text.len()));
    let head = text[..split_at].trim().to_string();
    let tail_start = crate::shared::floor_char_boundary(text, split_at.saturating_sub(overlap));
    let tail = text[tail_start..].trim().to_string();

    let mut out = vec![head];
    if !tail.is_empty() && tail.len() < text.len() {
        out.extend(recursive_char_chunks(&tail, max_chars, overlap));
    }
    out
}
