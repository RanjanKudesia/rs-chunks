//! `.mbox` splitting. A mailbox is a concatenation of RFC822 messages, each
//! preceded by a `From ` line at column 0 (the "From_" envelope separator).
//! This matches the behaviour of the reference `mailbox` implementations: split
//! on every `^From ` line. Truly-unmunged mailboxes (where a body line happens
//! to start with `From `) are inherently ambiguous and cannot be perfectly
//! recovered — we accept the reference behaviour and document the limitation.

use super::extract::{document_to_markdown, parse_message_bytes};

const FROM_SEP: &[u8] = b"From ";

/// Split raw mbox bytes into individual message byte-slices (the `From_`
/// separator line itself is dropped; mboxrd `>From` escaping is undone).
pub fn split_mbox(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut messages: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut started = false;

    for line in split_keep_eol(raw) {
        if line_starts_with(line, FROM_SEP) {
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
pub fn mbox_to_markdown(raw: &[u8]) -> (String, Vec<(String, Vec<u8>)>, usize) {
    let messages = split_mbox(raw);
    let count = messages.len();
    let mut out = String::new();
    let mut images: Vec<(String, Vec<u8>)> = Vec::new();

    out.push_str(&format!(
        "# Mailbox — {count} message{}\n\n",
        if count == 1 { "" } else { "s" }
    ));

    for (i, msg_bytes) in messages.iter().enumerate() {
        let doc = parse_message_bytes(msg_bytes);
        out.push_str(&format!("## Message {}\n\n", i + 1));
        out.push_str(&document_to_markdown(&doc, 3));
        out.push_str("\n\n");
        // Namespace image filenames by message index to avoid collisions.
        for (name, bytes) in doc.images {
            images.push((format!("msg{}_{name}", i + 1), bytes));
        }
    }

    (out.trim().to_string(), images, count)
}
