//! MIME email (`.eml`) extraction: parse via `mail-parser`, assemble a clean
//! markdown document (header block + body + attachments), mirroring the `.msg`
//! pipeline so both email formats share output shape and the Markdown chunker.

use mail_parser::{Address, Encoding, Message, MessageParser, MimeHeaders, PartType};

use crate::formats::html::to_markdown::html_to_markdown_str;

#[derive(Default, Clone)]
pub struct EmlAttachment {
    pub filename: Option<String>,
    pub mime: Option<String>,
    /// Byte size of the decoded part; parsed for completeness, not yet surfaced.
    #[allow(dead_code)]
    pub size: usize,
    /// Extracted text for nested `message/rfc822` attachments.
    pub embedded_text: Option<String>,
}

#[derive(Default, Clone)]
pub struct EmlDocument {
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub date: Option<String>,
    pub message_id: Option<String>,
    pub body: String,
    pub attachments: Vec<EmlAttachment>,
    /// Inline / attached images as (filename, bytes) for `list_images`.
    pub images: Vec<(String, Vec<u8>)>,
}

/// Format an address header into `Name <email>` / `email` display strings.
fn format_addresses(addr: Option<&Address>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(addr) = addr else { return out };
    for a in addr.iter() {
        let name = a.name().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let email = a
            .address()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        match (name, email) {
            (Some(n), Some(e)) => out.push(format!("{n} <{e}>")),
            (None, Some(e)) => out.push(e),
            (Some(n), None) => out.push(n),
            (None, None) => {}
        }
    }
    out
}

/// Select the best readable body: prefer text/plain, then HTML→markdown, then a
/// last-resort concatenation of any text parts (covers delivery-status reports).
fn select_body(msg: &Message) -> String {
    // When mail-parser renders an HTML part to text (an HTML-only message), its
    // entity handling differs across build targets: native decodes `&amp;`/
    // `&quot;`, wasm leaves them encoded. Detect that case and decode entities
    // ourselves so both targets match. Genuine text/plain bodies carry no
    // entities, so decoding is a no-op there (and stays identical to native).
    let primary_text_is_html = msg
        .text_body
        .first()
        .and_then(|id| msg.parts.get(*id as usize))
        .map(|p| matches!(p.body, PartType::Html(_)))
        .unwrap_or(false);

    if let Some(text) = msg.body_text(0) {
        let t = text.trim();
        if !t.is_empty() {
            // The declared charset can be wrong; check the result before
            // trusting it. (#72)
            if let Some(fixed) = redecode_if_charset_mismatched(msg, t) {
                return fixed;
            }
            return if primary_text_is_html {
                crate::formats::html::common::decode_html_entities(t)
            } else {
                t.to_string()
            };
        }
    }
    if let Some(html) = msg.body_html(0) {
        let md = html_to_markdown_str(&html);
        if !md.trim().is_empty() {
            return md.trim().to_string();
        }
    }
    // Fallback: gather every text part in document order.
    let mut collected = String::new();
    for part in &msg.parts {
        if let PartType::Text(t) = &part.body {
            let t = t.trim();
            if !t.is_empty() {
                if !collected.is_empty() {
                    collected.push_str("\n\n");
                }
                collected.push_str(t);
            }
        }
    }
    collected
}

fn image_ext(mime: Option<&str>) -> &'static str {
    match mime {
        Some(m) if m.contains("png") => ".png",
        Some(m) if m.contains("jpeg") || m.contains("jpg") => ".jpg",
        Some(m) if m.contains("gif") => ".gif",
        Some(m) if m.contains("webp") => ".webp",
        Some(m) if m.contains("bmp") => ".bmp",
        _ => ".bin",
    }
}

/// Parse a single RFC822 message (bytes) into an `EmlDocument`. Best-effort:
/// even a mostly-malformed message yields whatever headers/body parsed.
pub fn parse_message_bytes(raw: &[u8]) -> EmlDocument {
    // `mail-parser` can panic on some adversarial MIME structures (e.g. an
    // invalid multipart part-id). Under PyO3 that panic was caught at the
    // boundary; as a native library we must contain it ourselves so a single
    // malformed message never aborts the host. Falls back to an empty doc.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        match MessageParser::default().parse(raw) {
            // `mail-parser` parses parts lazily, so part access inside
            // `document_from_message` can also panic — keep it inside the guard.
            Some(msg) => document_from_message(&msg),
            None => EmlDocument::default(),
        }
    }));
    result.unwrap_or_default()
}

/// Attachment text worth inlining, or `None`.
///
/// A `text/*` content type is not a promise. `mimekit_missing-subtype.eml`
/// carries a gzip named `document.xml.gz` under a `text/` type, and inlining
/// its bytes would inject binary noise into the chunk. Require the decoded text
/// to be overwhelmingly printable before trusting the label.
fn readable_attachment_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let total = trimmed.chars().count();
    let printable = trimmed
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .count();
    // 5% slack for the odd stray control character in real mail.
    if printable * 20 < total * 19 || trimmed.contains('\u{0}') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Fraction of a string that is U+FFFD. A charset mismatch produces these in
/// bulk; ordinary text has none.
fn replacement_ratio(text: &str) -> f32 {
    let total = text.chars().count();
    if total == 0 {
        return 0.0;
    }
    text.chars().filter(|c| *c == '\u{fffd}').count() as f32 / total as f32
}

/// Re-decode a body whose declared charset clearly did not match the bytes.
///
/// `mimekit_jwz.mbox` has a Japanese message declaring `charset=ISO-2022-JP`
/// while the bytes on disk are UTF-8; honouring the label produced 687
/// replacement characters from a file with only 16 invalid sequences of its
/// own. A declared charset is a claim, and this is the same rule
/// `text_encoding.rs` already applies to `.txt` — verify before trusting. (#72)
///
/// Only untransformed bodies are retried; base64/quoted-printable ones have
/// already been transfer-decoded and re-deriving them here would be wrong.
fn redecode_if_charset_mismatched(msg: &Message<'_>, decoded: &str) -> Option<String> {
    if replacement_ratio(decoded) < 0.05 {
        return None;
    }
    let raw = msg.raw_message();
    let part = msg
        .text_body
        .first()
        .and_then(|id| msg.parts.get(*id as usize))?;
    if !matches!(part.encoding, Encoding::None) {
        return None;
    }
    let (from, to) = (part.offset_body as usize, part.offset_end as usize);
    let slice = raw.get(from..to.min(raw.len()))?;
    let text = std::str::from_utf8(slice).ok()?;
    // Only take the retry if it is genuinely cleaner.
    (replacement_ratio(text) < replacement_ratio(decoded)).then(|| text.trim().to_string())
}

fn full_content_type(part: &mail_parser::MessagePart) -> Option<String> {
    part.content_type().map(|ct| match ct.subtype() {
        Some(sub) => format!("{}/{}", ct.ctype(), sub),
        None => ct.ctype().to_string(),
    })
}

pub fn document_from_message(msg: &Message) -> EmlDocument {
    let mut doc = EmlDocument {
        subject: msg.subject().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        from: format_addresses(msg.from()).into_iter().next(),
        to: format_addresses(msg.to()),
        cc: format_addresses(msg.cc()),
        bcc: format_addresses(msg.bcc()),
        date: msg.date().map(|d| d.to_rfc3339()),
        message_id: msg.message_id().map(|s| s.to_string()),
        body: select_body(msg),
        ..Default::default()
    };

    // Attachments (incl. nested messages and images).
    let mut img_counter = 0usize;
    for part in msg.attachments() {
        match &part.body {
            PartType::Message(nested) => {
                let nested_doc = document_from_message(nested);
                let nested_md = document_to_markdown(&nested_doc, 3);
                doc.attachments.push(EmlAttachment {
                    filename: part.attachment_name().map(|s| s.to_string()),
                    mime: Some("message/rfc822".to_string()),
                    size: nested_md.len(),
                    embedded_text: (!nested_md.is_empty()).then_some(nested_md),
                });
                // Carry nested images up.
                for img in nested_doc.images {
                    doc.images.push(img);
                }
            }
            PartType::Binary(bytes) | PartType::InlineBinary(bytes) => {
                let mime = full_content_type(part);
                let is_image = mime.as_deref().map(|m| m.starts_with("image/")).unwrap_or(false);
                if is_image {
                    let ext = image_ext(mime.as_deref());
                    let name = part
                        .attachment_name()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            img_counter += 1;
                            format!("image_{img_counter}{ext}")
                        });
                    doc.images.push((name, bytes.to_vec()));
                }
                doc.attachments.push(EmlAttachment {
                    filename: part.attachment_name().map(|s| s.to_string()),
                    mime,
                    size: bytes.len(),
                    embedded_text: None,
                });
            }
            PartType::Text(t) => {
                // A text attachment's content is content. It was decoded and
                // then thrown away, so only the filename line rendered. (#35)
                doc.attachments.push(EmlAttachment {
                    filename: part.attachment_name().map(|s| s.to_string()),
                    mime: full_content_type(part),
                    size: t.len(),
                    embedded_text: readable_attachment_text(t.as_ref()),
                });
            }
            PartType::Html(t) => {
                // An HTML attachment goes through the same converter the HTML
                // *body* uses. Inlining the raw markup would dump a DOCTYPE and
                // a stylesheet into the chunk instead of the words. (#35)
                let text = html_to_markdown_str(t.as_ref());
                doc.attachments.push(EmlAttachment {
                    filename: part.attachment_name().map(|s| s.to_string()),
                    mime: full_content_type(part),
                    size: t.len(),
                    embedded_text: readable_attachment_text(&text),
                });
            }
            _ => {}
        }
    }

    // Inline images referenced by Content-ID that aren't flagged as attachments.
    for part in &msg.parts {
        if let PartType::InlineBinary(bytes) = &part.body {
            let mime = full_content_type(part);
            if mime.as_deref().map(|m| m.starts_with("image/")).unwrap_or(false) {
                let already = doc.images.iter().any(|(_, b)| b.as_slice() == bytes.as_ref());
                if !already {
                    let ext = image_ext(mime.as_deref());
                    img_counter += 1;
                    doc.images.push((format!("inline_{img_counter}{ext}"), bytes.to_vec()));
                }
            }
        }
    }

    doc
}

/// Assemble the markdown document. `heading_level` controls the subject heading
/// depth (1 for a top-level `.eml`, deeper for nested messages).
pub fn document_to_markdown(doc: &EmlDocument, heading_level: usize) -> String {
    let mut out = String::new();
    let hashes = "#".repeat(heading_level.clamp(1, 6));
    if let Some(subject) = &doc.subject {
        out.push_str(&format!("{hashes} {subject}\n\n"));
    }

    let mut hdr: Vec<String> = Vec::new();
    if let Some(from) = &doc.from {
        hdr.push(format!("**From:** {from}"));
    }
    if !doc.to.is_empty() {
        hdr.push(format!("**To:** {}", doc.to.join(", ")));
    }
    if !doc.cc.is_empty() {
        hdr.push(format!("**Cc:** {}", doc.cc.join(", ")));
    }
    if let Some(d) = &doc.date {
        hdr.push(format!("**Date:** {d}"));
    }
    if !hdr.is_empty() {
        out.push_str(&hdr.join("  \n"));
        out.push_str("\n\n");
    }

    if !doc.body.is_empty() {
        out.push_str(&doc.body);
        out.push_str("\n\n");
    }

    if !doc.attachments.is_empty() {
        out.push_str("## Attachments\n\n");
        for att in &doc.attachments {
            let name = att.filename.clone().unwrap_or_else(|| "(unnamed)".to_string());
            let mime = att.mime.as_ref().map(|m| format!(" ({m})")).unwrap_or_default();
            out.push_str(&format!("- {name}{mime}\n"));
            if let Some(text) = &att.embedded_text {
                out.push_str(&format!("\n{text}\n"));
            }
        }
    }

    out.trim().to_string()
}
