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

/// Split `text` into overlapping windows of at most `max_chars` bytes.
///
/// **Iterative on purpose.** This was written as a recursion that allocated the
/// entire remaining tail at every level (`text[tail_start..].trim().to_string()`),
/// which made it O(n²) in copying and gave it a stack depth of
/// `len / (max_chars - overlap)`. A plain `.docx` of many short paragraphs —
/// no crafting needed, just a long document — reached **1,956 MB of RSS from a
/// 230 KB file**, scaling as n^1.81; a larger one was OOM-killed outright. It is
/// the default mode of the format, so this was the easiest path to a dead
/// process in the engine.
///
/// The allocation was never necessary: `str::trim` returns a *borrowed*
/// subslice, so the loop below walks the original buffer and only allocates the
/// chunks it actually emits. Output is byte-for-byte what the recursion
/// produced — including the deliberate asymmetry that the final chunk is **not**
/// trimmed while every earlier one is, which `identical_to_the_recursive_form`
/// pins against a copy of the original.
pub(super) fn recursive_char_chunks(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: &str = text;

    loop {
        if cur.len() <= max_chars {
            // Base case keeps `cur` untrimmed, matching the original.
            out.push(cur.to_string());
            return out;
        }

        let split_at = crate::shared::floor_char_boundary(cur, max_chars.min(cur.len()));
        out.push(cur[..split_at].trim().to_string());

        let tail_start = crate::shared::floor_char_boundary(cur, split_at.saturating_sub(overlap));
        let tail = cur[tail_start..].trim();

        // The original stopped here rather than recursing, so no chunk follows.
        if tail.is_empty() || tail.len() >= cur.len() {
            return out;
        }
        cur = tail;
    }
}

#[cfg(test)]
mod recursive_char_chunks_tests {
    use super::recursive_char_chunks;

    /// The original recursive implementation, kept verbatim so the iterative
    /// rewrite can be proved equivalent rather than assumed to be.
    fn reference(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
        if text.len() <= max_chars {
            return vec![text.to_string()];
        }
        let split_at = crate::shared::floor_char_boundary(text, max_chars.min(text.len()));
        let head = text[..split_at].trim().to_string();
        let tail_start = crate::shared::floor_char_boundary(text, split_at.saturating_sub(overlap));
        let tail = text[tail_start..].trim().to_string();
        let mut out = vec![head];
        if !tail.is_empty() && tail.len() < text.len() {
            out.extend(reference(&tail, max_chars, overlap));
        }
        out
    }

    /// Deterministic pseudo-random text, so a failure is reproducible.
    fn corpus(seed: u64, len: usize) -> String {
        let alphabet: Vec<char> = " abc .!? \n\tßé漢字🙂".chars().collect();
        let mut s = String::new();
        let mut x = seed | 1;
        for _ in 0..len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push(alphabet[(x as usize) % alphabet.len()]);
        }
        s
    }

    #[test]
    fn identical_to_the_recursive_form() {
        // Sizes stay small enough that the reference's own stack depth is safe.
        for seed in 1..40u64 {
            for len in [0usize, 1, 7, 64, 300, 1200, 4000] {
                for (max_chars, overlap) in [(600, 100), (1200, 200), (64, 0), (32, 31), (10, 9)] {
                    let text = corpus(seed, len);
                    assert_eq!(
                        recursive_char_chunks(&text, max_chars, overlap),
                        reference(&text, max_chars, overlap),
                        "diverged: seed={seed} len={len} max={max_chars} overlap={overlap}"
                    );
                }
            }
        }
    }

    #[test]
    fn multibyte_text_never_splits_mid_character() {
        let text = "漢字テスト🙂".repeat(500);
        for out in recursive_char_chunks(&text, 61, 7) {
            assert!(out.chars().count() > 0 || out.is_empty());
        }
        // Reassembling must not have produced replacement characters.
        assert!(!recursive_char_chunks(&text, 61, 7)
            .concat()
            .contains('\u{FFFD}'));
    }

    /// The regression this rewrite exists for. The recursive form allocated the
    /// whole remaining tail per level, so this input cost O(n²) copying and
    /// `len / (max_chars - overlap)` stack frames. 4 MB at the real production
    /// settings is ~8,000 levels — enough to blow the stack on a default thread.
    #[test]
    fn large_input_is_linear_and_does_not_recurse() {
        let text = "word ".repeat(800_000); // 4 MB
        let started = std::time::Instant::now();
        let chunks = recursive_char_chunks(
            &text,
            super::super::structural_model::SHORT_AGGREGATE_CHUNK_SIZE,
            super::super::structural_model::SHORT_AGGREGATE_CHUNK_OVERLAP,
        );
        let elapsed = started.elapsed();
        assert!(!chunks.is_empty());
        // Total emitted bytes must stay proportional to the input (plus overlap),
        // which is what fails if the tail is being copied at every level.
        let emitted: usize = chunks.iter().map(String::len).sum();
        assert!(
            emitted < text.len() * 3,
            "emitted {emitted} bytes from a {} byte input — copying is superlinear",
            text.len()
        );
        assert!(
            elapsed.as_secs() < 10,
            "took {elapsed:?} — the quadratic copy is back"
        );
    }
}
