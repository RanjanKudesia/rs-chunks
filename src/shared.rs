/// Shared utilities used across all chunking format modules.
///
/// Single source of truth for stop-words, keyword helpers, sentence splitting,
/// and semantic-signal constants so batch and streaming paths are always in sync.
use std::collections::HashSet;

// ── Stop-word list ────────────────────────────────────────────────────────────

pub const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "this", "that", "with", "from", "have", "been",
    "will", "they", "their", "which", "about", "into", "more", "also", "when", "than", "those",
    "these", "each", "such", "some", "can", "all", "any", "but", "not", "its", "has", "had", "you",
    "your", "our", "use", "used", "using", "may", "must", "should", "would", "could", "both",
    "over", "after", "then", "there", "here", "only", "just", "even", "very", "well", "now", "new",
    "way", "get", "set", "let", "run", "see", "per", "via", "etc", "one", "two", "three", "four",
    "five", "first", "last", "next", "same", "other", "while", "where", "how", "what", "who",
];

// ── Semantic merge-signal constants ───────────────────────────────────────────
// Single canonical source used by every format's semantic chunker.
// Batch and streaming paths import from here so they can never diverge.

pub const TRANSITION_BREAKS: &[&str] = &[
    "however",
    "nevertheless",
    "in contrast",
    "on the other hand",
    "meanwhile",
    "conversely",
    "that said",
    "in summary",
    "to summarize",
    "to conclude",
    "in conclusion",
    "to wrap up",
    "overall",
    "in closing",
];

/// Starts with a reference pronoun — signals the paragraph continues the
/// previous referent.  Entries deliberately include a trailing space so a
/// plain `starts_with` check avoids false matches on e.g. "theater".
pub const REFERENCE_STARTS: &[&str] = &[
    "this ",
    "it ",
    "they ",
    "these ",
    "that ",
    "those ",
    "its ",
    "their ",
    "such ",
    "the above",
    "the following",
    "the latter",
    "the former",
];

pub const ELABORATION_STARTS: &[&str] = &[
    "additionally",
    "furthermore",
    "moreover",
    "in addition",
    "what is more",
    "on top of that",
    "notably",
    "importantly",
    "it is worth",
    "it should be noted",
    "equally",
    "similarly",
    "likewise",
];

pub const EXAMPLE_STARTS: &[&str] = &[
    "for example",
    "for instance",
    "such as",
    "e.g.",
    "i.e.",
    "as an example",
    "to illustrate",
    "consider ",
    "as shown",
    "as seen",
    "as demonstrated",
    "take ",
    "imagine ",
];

pub const CAUSE_EFFECT_STARTS: &[&str] = &[
    "because",
    "therefore",
    "thus",
    "hence",
    "as a result",
    "consequently",
    "this means",
    "this leads",
    "this causes",
    "this results",
    "this implies",
    "this suggests",
    "so ",
];

pub const CONTRAST_CONTINUATION: &[&str] = &[
    "although",
    "even though",
    "despite",
    "whereas",
    "even if",
    "regardless",
    "notwithstanding",
    "while it",
    "while this",
];

/// Maximum chars for a semantic chunk.  Single source of truth — imported by
/// every semantic chunker so batch and streaming paths can never diverge.
pub const MAX_SEMANTIC_CHARS: usize = 1500;

/// Paragraphs at or below this length are candidates for absorption into the
/// previous chunk (signal 9 — short_paragraph).
pub const SHORT_PARA_CHARS: usize = 80;

// ── Case-insensitive prefix helper ────────────────────────────────────────────

/// Returns `true` if `text` starts with `prefix` (ASCII case-insensitive).
///
/// Uses `str::get` so it never panics on non-char-boundary slices.
#[inline]
/// Largest index <= `idx` that is a UTF-8 char boundary of `s`. Prevents a
/// panic when splitting a string on a byte budget that lands inside a multibyte
/// character (e.g. Cyrillic/CJK text at a chunk-size cutoff).
pub fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn ci_starts_with(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .map(|s| s.eq_ignore_ascii_case(prefix))
        .unwrap_or(false)
}

// ── Keyword helpers ───────────────────────────────────────────────────────────

pub fn tokenize_keywords(text: &str) -> HashSet<String> {
    let lower = text.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 4 && !STOPWORDS.contains(w))
        .map(|s| s.to_string())
        .collect()
}

pub fn has_keyword_overlap(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.intersection(b).next().is_some()
}

// ── Sentence splitting ────────────────────────────────────────────────────────

/// Splits `text` into sentences at `.`, `!`, or `?` followed by whitespace.
///
/// Uses a `Peekable<Chars>` iterator — avoids the `Vec<char>` heap allocation
/// that the old implementations used.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().map(|n| n.is_whitespace()).unwrap_or(false)
        {
            let s = current.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            current.clear();
        }
    }
    let tail = current.trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// Splits `text` into chunks of at most `max_chars`, breaking at sentence
/// boundaries where possible.
/// Split a block that is too large to be one chunk, on line boundaries.
///
/// Paragraphs were already bounded by [`split_at_sentences`], but lists, tables
/// and code blocks were emitted whole however big they got. A single TopoJSON
/// object rendered as one bullet list produced a 764,655-character chunk —
/// orders of magnitude past what any embedding model accepts, which makes the
/// chunk useless for the one job this library exists to do.
///
/// Lines are never broken mid-way, so a list item or table row stays intact. A
/// single line longer than `max_chars` is emitted alone rather than mangled.
/// `repeat_prefix` re-emits the leading lines (a table's header and separator)
/// at the top of every part, so each part stays readable on its own.
/// Split a block on line boundaries, then split any single line that is still
/// over the cap at its sentence boundaries.
///
/// `split_block_on_lines` deliberately never breaks a line, so one very long
/// line came out alone and over the cap ([#45](TECH_DEBT.md) bounded the block,
/// [#68](TECH_DEBT.md) is the residue). Prose can be cut at a sentence without
/// losing meaning, so it is — and a line with no sentence boundary at all is
/// hard-split at a UTF-8 boundary, because the alternative is an unbounded
/// chunk.
///
/// Only the caller knows whether a line is divisible: a table row, a CSV
/// record and a rendered JSON object are single units whose halves mean
/// nothing, and `table` is documented as "kept whole". So this is a separate
/// function rather than a change to `split_block_on_lines`.
pub fn split_block_on_lines_and_sentences(content: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    for part in split_block_on_lines(content, max_chars, 0) {
        if part.chars().count() <= max_chars {
            out.push(part);
            continue;
        }
        for piece in split_at_sentences(&part, max_chars) {
            if piece.chars().count() <= max_chars {
                if !piece.trim().is_empty() {
                    out.push(piece);
                }
                continue;
            }
            out.extend(hard_split(&piece, max_chars));
        }
    }
    if out.is_empty() {
        out.push(content.to_string());
    }
    out
}

/// Last resort for text with no sentence boundary at all — a minified JSON
/// line, a base64 blob, a run of hyphenated tokens.
///
/// Breaks at the last word boundary in the window rather than at the character
/// the cap happens to land on, so `banana` does not come out as `bana` + `na`.
/// Falls back to a character cut only when the window holds no whitespace at
/// all, which is the genuinely indivisible case. Cuts are always on UTF-8 char
/// boundaries, so a multi-byte character is never halved.
fn hard_split(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.chars().count() > max_chars {
        // Byte index one past the `max_chars`-th character.
        let cut = rest
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let window = &rest[..cut];
        let split_at = window
            .rfind(char::is_whitespace)
            .filter(|i| *i > 0)
            .unwrap_or(cut);
        let (head, tail) = rest.split_at(split_at);
        if !head.trim().is_empty() {
            out.push(head.trim_end().to_string());
        }
        rest = tail.trim_start();
        if rest.is_empty() {
            break;
        }
    }
    if !rest.trim().is_empty() {
        out.push(rest.to_string());
    }
    out
}

pub fn split_block_on_lines(content: &str, max_chars: usize, repeat_prefix: usize) -> Vec<String> {
    if content.chars().count() <= max_chars {
        return vec![content.to_string()];
    }
    let lines: Vec<&str> = content.lines().collect();
    let prefix: Vec<&str> = lines.iter().take(repeat_prefix).copied().collect();
    let prefix_len: usize = prefix.iter().map(|l| l.chars().count() + 1).sum();

    let mut parts: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = prefix.clone();
    let mut cur_len = prefix_len;

    for line in lines.iter().skip(repeat_prefix) {
        let add = line.chars().count() + 1;
        if cur_len + add > max_chars && cur.len() > prefix.len() {
            parts.push(cur.join("\n"));
            cur = prefix.clone();
            cur_len = prefix_len;
        }
        cur.push(line);
        cur_len += add;
    }
    if cur.len() > prefix.len() || parts.is_empty() {
        parts.push(cur.join("\n"));
    }
    parts
}

pub fn split_at_sentences(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.trim().to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for sentence in split_sentences(text) {
        let candidate = if current.is_empty() {
            sentence.clone()
        } else {
            format!("{} {}", current, sentence)
        };
        if candidate.len() <= max_chars {
            current = candidate;
        } else {
            if !current.is_empty() {
                out.push(current.trim().to_string());
            }
            current = sentence;
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    if out.is_empty() {
        vec![text.trim().to_string()]
    } else {
        out.into_iter().filter(|c| !c.is_empty()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── ci_starts_with ────────────────────────────────────────────────────────

    #[test]
    fn ci_starts_with_exact_match() {
        assert!(ci_starts_with("Hello World", "Hello"));
    }

    #[test]
    fn ci_starts_with_case_insensitive() {
        assert!(ci_starts_with("HELLO world", "hello"));
        assert!(ci_starts_with("hello world", "HELLO"));
    }

    #[test]
    fn ci_starts_with_false_when_no_match() {
        assert!(!ci_starts_with("world", "Hello"));
    }

    #[test]
    fn ci_starts_with_empty_prefix_always_true() {
        assert!(ci_starts_with("anything", ""));
        assert!(ci_starts_with("", ""));
    }

    #[test]
    fn ci_starts_with_prefix_longer_than_text() {
        assert!(!ci_starts_with("hi", "hello"));
    }

    // ── tokenize_keywords ─────────────────────────────────────────────────────

    #[test]
    fn tokenize_keywords_filters_stopwords() {
        let kws = tokenize_keywords("the and for are with");
        assert!(
            kws.is_empty(),
            "expected no keywords from stopwords, got {:?}",
            kws
        );
    }

    #[test]
    fn tokenize_keywords_filters_short_tokens() {
        // Words < 4 chars are dropped even if not stopwords.
        let kws = tokenize_keywords("a an be it");
        assert!(kws.is_empty());
    }

    #[test]
    fn tokenize_keywords_extracts_meaningful_words() {
        let kws = tokenize_keywords("machine learning algorithm");
        assert!(kws.contains("machine"));
        assert!(kws.contains("learning"));
        assert!(kws.contains("algorithm"));
    }

    #[test]
    fn tokenize_keywords_lowercases_output() {
        let kws = tokenize_keywords("MACHINE Learning");
        assert!(kws.contains("machine"));
        assert!(kws.contains("learning"));
        assert!(!kws.contains("MACHINE"));
    }

    #[test]
    fn tokenize_keywords_splits_on_punctuation() {
        let kws = tokenize_keywords("machine,learning;algorithm");
        assert!(kws.contains("machine"));
        assert!(kws.contains("algorithm"));
    }

    // ── has_keyword_overlap ───────────────────────────────────────────────────

    #[test]
    fn has_keyword_overlap_returns_true_on_shared_keyword() {
        let a = tokenize_keywords("machine learning rocks");
        let b = tokenize_keywords("learning something interesting");
        assert!(has_keyword_overlap(&a, &b));
    }

    #[test]
    fn has_keyword_overlap_returns_false_when_disjoint() {
        let a = tokenize_keywords("machine learning something");
        let b = tokenize_keywords("quantum physics experiment");
        assert!(!has_keyword_overlap(&a, &b));
    }

    #[test]
    fn has_keyword_overlap_returns_false_when_either_empty() {
        let empty: HashSet<String> = HashSet::new();
        let b = tokenize_keywords("learning");
        assert!(!has_keyword_overlap(&empty, &b));
        assert!(!has_keyword_overlap(&b, &empty));
    }

    // ── split_sentences ───────────────────────────────────────────────────────

    #[test]
    fn split_sentences_splits_on_period_space() {
        let s = split_sentences("Hello world. How are you. Fine.");
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], "Hello world.");
        assert_eq!(s[1], "How are you.");
    }

    #[test]
    fn split_sentences_handles_question_and_exclamation() {
        let s = split_sentences("Really? Yes! Great.");
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn split_sentences_tail_without_terminal_punctuation() {
        let s = split_sentences("First sentence. trailing text");
        assert_eq!(s.len(), 2);
        assert_eq!(s[1], "trailing text");
    }

    #[test]
    fn split_sentences_no_trailing_empty_strings() {
        let s = split_sentences("One sentence.");
        assert_eq!(s.len(), 1);
        assert!(!s[0].is_empty());
    }

    // ── split_at_sentences ────────────────────────────────────────────────────

    #[test]
    fn split_at_sentences_single_when_fits() {
        let r = split_at_sentences("Short text.", 1000);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], "Short text.");
    }

    #[test]
    fn split_at_sentences_splits_on_boundary() {
        let text = "A".repeat(80) + ". " + &"B".repeat(80) + ".";
        let r = split_at_sentences(&text, 90);
        assert!(r.len() >= 2, "expected split, got {} chunks", r.len());
    }

    #[test]
    fn split_at_sentences_no_empty_chunks() {
        let text = "First sentence. Second sentence. Third sentence.";
        let r = split_at_sentences(text, 25);
        assert!(r.iter().all(|c| !c.is_empty()));
    }
}

#[cfg(test)]
mod oversized_line_tests {
    use super::*;

    const CAP: usize = 40;

    /// TECH_DEBT #68. A single line over the cap used to be emitted whole.
    #[test]
    fn an_oversized_line_is_split_at_its_sentences() {
        let text = "First sentence here. Second sentence here. Third sentence here. Fourth one.";
        let parts = split_block_on_lines_and_sentences(text, CAP);
        assert!(parts.len() > 1, "expected a split, got {parts:?}");
        assert!(parts.iter().all(|p| p.chars().count() <= CAP), "{parts:?}");
        assert!(parts.join(" ").contains("Fourth one."));
    }

    /// Text with no sentence boundary still has to be bounded — but it must
    /// break between words, not through one.
    #[test]
    fn a_line_without_sentences_breaks_on_word_boundaries() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu";
        let parts = split_block_on_lines_and_sentences(text, CAP);
        assert!(parts.iter().all(|p| p.chars().count() <= CAP), "{parts:?}");
        for word in ["alpha", "epsilon", "lambda", "theta"] {
            assert!(
                parts
                    .iter()
                    .any(|p| p.split_whitespace().any(|w| w == word)),
                "{word} was cut in half: {parts:?}"
            );
        }
    }

    /// A run with no whitespace anywhere — base64, a minified array — is the
    /// genuinely indivisible case and falls back to a character cut.
    #[test]
    fn a_run_with_no_whitespace_still_gets_bounded() {
        let text = "x".repeat(CAP * 3 + 7);
        let parts = split_block_on_lines_and_sentences(&text, CAP);
        assert!(parts.iter().all(|p| p.chars().count() <= CAP), "{parts:?}");
        assert_eq!(parts.concat().len(), text.len(), "no character was dropped");
    }

    /// Multi-byte text must never be cut through a character.
    #[test]
    fn multibyte_text_is_never_halved() {
        let text = "日本語の段落です。".repeat(20);
        let parts = split_block_on_lines_and_sentences(&text, CAP);
        assert!(parts.iter().all(|p| p.chars().count() <= CAP), "{parts:?}");
        assert!(!parts.concat().contains('\u{FFFD}'));
    }

    #[test]
    fn a_block_already_within_the_cap_is_untouched() {
        let text = "short enough";
        assert_eq!(split_block_on_lines_and_sentences(text, CAP), vec![text]);
    }
}
