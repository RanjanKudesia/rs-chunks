//! `.mbox` splitting. A mailbox is a concatenation of RFC822 messages, each
//! preceded by a `From ` line at column 0 (the "From_" envelope separator).
//! This matches the behaviour of the reference `mailbox` implementations: split
//! on every `^From ` line. Truly-unmunged mailboxes (where a body line happens
//! to start with `From `) are inherently ambiguous and cannot be perfectly
//! recovered — we accept the reference behaviour and document the limitation.

use super::extract::{document_to_markdown, parse_message_bytes};

const FROM_SEP: &[u8] = b"From ";

/// Does this line look like the START of an RFC 5322 header block?
///
/// A genuine `From_` postmark is always followed immediately by message
/// headers (`Return-Path:`, `Received:`, …): a field name of printable ASCII
/// followed by a colon. A BODY line that merely begins with `From ` is
/// followed by more prose. This is the discriminator that keeps
/// `From Russia with love` in a message body from becoming a phantom message
/// — which is exactly what happened: `mimekit_unmunged.mbox` holds 2 messages
/// and the engine emitted 4, each phantom's "headers" being prose, and the
/// mis-detected line was then DELETED from the real message's body.
fn looks_like_header_start(line: &[u8]) -> bool {
    let mut saw_name = false;
    for (i, &b) in line.iter().take(100).enumerate() {
        match b {
            b':' => return saw_name && i > 0,
            // ftext: printable US-ASCII except colon.
            33..=57 | 59..=126 => saw_name = true,
            _ => return false,
        }
    }
    false
}

/// Does the text after `From ` look like a postmark tail?
///
/// RFC 4155's shape is `From sender date`, and every real writer follows it:
/// classic mailers emit an asctime date (`Mon Jun 01 04:28:28 2009` — weekday
/// plus a time containing colons), Thunderbird emits a bare `-`. Prose that
/// merely starts with `From ` has neither. Measured across the corpus: all
/// 162 genuine postmarks are `-` or carry an asctime tail; the two phantom
/// `Russia with love` lines have neither.
fn postmark_tail(line: &[u8]) -> bool {
    let tail: &[u8] = &line[FROM_SEP.len()..];
    let tail = trim_ascii(tail);
    tail.is_empty() || tail == b"-" || (tail.contains(&b':') && tail.iter().any(u8::is_ascii_digit))
}

fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

/// Split raw mbox bytes into individual message byte-slices (the `From_`
/// separator line itself is dropped; mboxrd `>From` escaping is undone).
pub fn split_mbox(raw: &[u8]) -> Vec<Vec<u8>> {
    let lines: Vec<&[u8]> = split_keep_eol(raw);
    let mut messages: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut started = false;

    for (idx, &line) in lines.iter().enumerate() {
        // A postmark must (a) itself look like one and (b) be followed by
        // headers. The very first line of the file is trusted regardless.
        // Both conditions are needed: the corpus's own adversarial fixture
        // follows a body `From Russia with love` with `Year: 1963` — a
        // header-shaped line — so next-line shape alone still phantom-splits.
        let next_is_headerish = lines
            .get(idx + 1)
            .map(|l| looks_like_header_start(l))
            .unwrap_or(false);
        if line_starts_with(line, FROM_SEP)
            && (idx == 0 || (postmark_tail(line) && next_is_headerish))
        {
            // New message boundary — flush the previous one.
            if started && !current.is_empty() {
                messages.push(std::mem::take(&mut current));
            }
            started = true;
            current.clear();
            continue; // drop the separator line itself
        }
        if !started {
            // Bytes before the first `From ` line (rare) — start a message anyway.
            started = true;
        }
        // mboxrd: a line of one-or-more '>' followed by "From " had a '>' added
        // on write; strip one leading '>' to restore the original.
        if is_escaped_from(line) {
            current.extend_from_slice(&line[1..]);
        } else {
            current.extend_from_slice(line);
        }
    }
    if !current.is_empty() {
        messages.push(current);
    }
    messages
}

fn line_starts_with(line: &[u8], prefix: &[u8]) -> bool {
    line.len() >= prefix.len() && &line[..prefix.len()] == prefix
}

/// True for lines matching `>+From ` (mboxrd-escaped `From ` lines).
fn is_escaped_from(line: &[u8]) -> bool {
    let mut i = 0;
    while i < line.len() && line[i] == b'>' {
        i += 1;
    }
    i > 0 && line_starts_with(&line[i..], FROM_SEP)
}

/// Split bytes into lines, keeping the trailing `\n` on each line so the message
/// bytes reconstruct faithfully.
fn split_keep_eol(raw: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in raw.iter().enumerate() {
        if b == b'\n' {
            out.push(&raw[start..=i]);
            start = i + 1;
        }
    }
    if start < raw.len() {
        out.push(&raw[start..]);
    }
    out
}

/// Assemble an mbox into one markdown document with a section per message, plus
/// the collected images and the message count.
/// What a `.mbox` chunk needs to say which message it came from.
///
/// A 152-message mailbox gave every chunk the same
/// `{source_type, message_count}` — the per-message envelope was parsed and
/// thrown away, so "which message is this?" was unanswerable. (#36)
#[derive(Clone)]
pub struct MboxMessageInfo {
    pub index: usize,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub date: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
}

/// What one mbox yielded. A struct rather than a fifth tuple element: the
/// return was already four anonymous values, and `skipped` only means anything
/// next to `count`.
pub struct MboxLoad {
    pub markdown: String,
    pub images: crate::chunk::ExtractedImages,
    /// Messages the splitter found — unchanged by any parse failure below.
    pub count: usize,
    pub infos: Vec<MboxMessageInfo>,
    /// Messages that could not be parsed, as `"message {n}: {reason}"`.
    ///
    /// Always present, empty when nothing was lost. One unparseable message
    /// must not lose a 5,000-message mailbox, but the gap it leaves in the
    /// `## Message N` numbering has to be explained rather than left as a blank
    /// heading. Same contract as xlsx's `skipped_sheets` (#66).
    pub skipped: Vec<String>,
}

pub fn mbox_to_markdown(raw: &[u8]) -> MboxLoad {
    let messages = split_mbox(raw);
    let count = messages.len();
    let mut out = String::new();
    let mut images: crate::chunk::ExtractedImages = Vec::new();

    out.push_str(&format!(
        "# Mailbox — {count} message{}\n\n",
        if count == 1 { "" } else { "s" }
    ));

    let mut infos: Vec<MboxMessageInfo> = Vec::with_capacity(count);
    let mut skipped: Vec<String> = Vec::new();
    for (i, msg_bytes) in messages.iter().enumerate() {
        let doc = match parse_message_bytes(msg_bytes) {
            Ok(doc) => doc,
            Err(e) => {
                skipped.push(format!("message {}: {e}", i + 1));
                continue;
            }
        };
        infos.push(MboxMessageInfo {
            index: i + 1,
            subject: doc.subject.clone(),
            from: doc.from.clone(),
            date: doc.date.clone(),
            message_id: doc.message_id.clone(),
            in_reply_to: doc.in_reply_to.clone(),
            references: doc.references.clone(),
        });
        out.push_str(&format!("## Message {}\n\n", i + 1));
        out.push_str(&document_to_markdown(&doc, 3));
        out.push_str("\n\n");
        // Namespace image filenames by message index to avoid collisions.
        for (name, bytes) in doc.images {
            images.push((format!("msg{}_{name}", i + 1), bytes));
        }
    }

    MboxLoad {
        markdown: out.trim().to_string(),
        images,
        count,
        infos,
        skipped,
    }
}
