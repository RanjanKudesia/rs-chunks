//! Outlook `.msg` (MS-OXMSG) extraction — a MAPI message reader.
//!
//! Reads the full envelope (subject, sender, To/Cc/Bcc recipient table, dates,
//! importance), the body (HTML → plain → RTF, in that quality order), item-type
//! specific fields for appointments/contacts/tasks (via the named-property map),
//! and attachments (metadata + recursive embedded-message text). See MS-OXMSG,
//! MS-OXPROPS, MS-OXRTFCP.

use std::io::Read;

use encoding_rs::{Encoding, BIG5, GBK, SHIFT_JIS, UTF_8, WINDOWS_1251, WINDOWS_1252};

use super::rtf::compressed_rtf_to_text;

type Cfb<'a> = cfb::CompoundFile<std::io::Cursor<&'a [u8]>>;

// ── Property ids (PidTag*, hex) ───────────────────────────────────────────────
const PID_SUBJECT: u16 = 0x0037;
const PID_MESSAGE_CLASS: u16 = 0x001A;
const PID_BODY: u16 = 0x1000;
const PID_HTML: u16 = 0x1013;
const PID_RTF_COMPRESSED: u16 = 0x1009;
const PID_SENDER_NAME: u16 = 0x0C1A;
const PID_SENDER_EMAIL: u16 = 0x0C1F;
const PID_SENDER_SMTP: u16 = 0x5D01;
/// PidTagSenderAddrType — "SMTP" for a real address, "EX" for an Exchange DN.
const PID_SENDER_ADDRTYPE: u16 = 0x0C1E;
/// PidTagRecipientAddrType, the per-recipient equivalent.
const PID_RECIP_ADDRTYPE: u16 = 0x3002;
const PID_DISPLAY_TO: u16 = 0x0E04;
const PID_DISPLAY_CC: u16 = 0x0E03;
const PID_DISPLAY_BCC: u16 = 0x0E02;
const PID_CONVERSATION_TOPIC: u16 = 0x0070;
const PID_CLIENT_SUBMIT_TIME: u16 = 0x0039;
const PID_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
const PID_IMPORTANCE: u16 = 0x0017;
const PID_CODEPAGE: u16 = 0x3FFD;
const PID_INTERNET_CODEPAGE: u16 = 0x3FDE;
// Recipient
const PID_RECIP_NAME: u16 = 0x3001;
const PID_RECIP_EMAIL: u16 = 0x3003;
const PID_RECIP_SMTP: u16 = 0x39FE;
const PID_RECIP_TYPE: u16 = 0x0C15;
// Attachment
const PID_ATTACH_LONG_FILENAME: u16 = 0x3707;
const PID_ATTACH_FILENAME: u16 = 0x3704;
const PID_ATTACH_MIME: u16 = 0x370E;
const PID_ATTACH_SIZE: u16 = 0x0E20;
const PID_ATTACH_METHOD: u16 = 0x3705;
const PID_ATTACH_DATA: u16 = 0x3701;

#[derive(Default)]
pub struct Attachment {
    pub filename: Option<String>,
    pub mime: Option<String>,
    /// PidTagAttachSize; parsed for completeness, not yet surfaced in metadata.
    #[allow(dead_code)]
    pub size: Option<u64>,
    pub embedded_text: Option<String>,
}

#[derive(Default)]
pub struct MsgDocument {
    pub message_class: String,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub sent_date: Option<String>,
    pub received_date: Option<String>,
    pub importance: Option<String>,
    pub conversation_topic: Option<String>,
    pub body: Option<String>,
    /// Item-type-specific (label, value) pairs, e.g. appointment when/where.
    pub item_fields: Vec<(String, String)>,
    pub attachments: Vec<Attachment>,
    /// Attached images as (filename, bytes) for `list_images`, mirroring `.eml`.
    pub images: Vec<(String, Vec<u8>)>,
}

// ── Low-level CFB / property access ───────────────────────────────────────────

fn read_stream(cfb: &mut Cfb<'_>, path: &str) -> Option<Vec<u8>> {
    let mut s = cfb.open_stream(path).ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn strip_nulls(s: &str) -> String {
    s.trim_end_matches('\u{0}').trim().to_string()
}

fn encoding_for_codepage(cp: u32) -> &'static Encoding {
    match cp {
        65001 => UTF_8,
        1251 => WINDOWS_1251,
        936 | 54936 => GBK,
        950 => BIG5,
        932 => SHIFT_JIS,
        _ => WINDOWS_1252, // incl. 1252 and 28591 (Latin-1)
    }
}

/// Decode an ANSI (001E) byte string in the message codepage. A codepage of
/// 65001 (UTF-8) is common as an *internet* codepage even when the single-byte
/// 001E stream is really cp1252 — so for 65001 we accept UTF-8 only if the bytes
/// are valid UTF-8, otherwise fall back to Windows-1252.
fn decode_ansi(bytes: &[u8], codepage: u32) -> String {
    if codepage == 65001 {
        if let Some(s) = UTF_8.decode_without_bom_handling_and_without_replacement(bytes) {
            return s.into_owned();
        }
        let (d, _, _) = WINDOWS_1252.decode(bytes);
        return d.into_owned();
    }
    let (d, _, _) = encoding_for_codepage(codepage).decode(bytes);
    d.into_owned()
}

/// Read a string property `<prefix>/__substg1.0_<pid><type>`, Unicode first.
fn read_string(cfb: &mut Cfb<'_>, prefix: &str, pid: u16, codepage: u32) -> Option<String> {
    let hex = format!("{pid:04X}");
    if let Some(bytes) = read_stream(cfb, &format!("{prefix}/__substg1.0_{hex}001F")) {
        let (d, _, _) = encoding_rs::UTF_16LE.decode(&bytes);
        let s = strip_nulls(&d);
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Some(bytes) = read_stream(cfb, &format!("{prefix}/__substg1.0_{hex}001E")) {
        let s = strip_nulls(&decode_ansi(&bytes, codepage));
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn read_binary(cfb: &mut Cfb<'_>, prefix: &str, pid: u16) -> Option<Vec<u8>> {
    read_stream(cfb, &format!("{prefix}/__substg1.0_{pid:04X}0102"))
}

/// Fixed-width property lookup in `<prefix>/__properties_version1.0`. Entries are
/// 16 bytes: `[2B type][2B pid][4B flags][8B value]`. `header` is 32 at the top
/// level, 24 inside recipient/attachment/embedded sub-storages.
fn read_fixed_prop(cfb: &mut Cfb<'_>, prefix: &str, pid: u16, header: usize) -> Option<(u16, [u8; 8])> {
    let data = read_stream(cfb, &format!("{prefix}/__properties_version1.0"))?;
    let mut off = header;
    while off + 16 <= data.len() {
        let typ = u16::from_le_bytes([data[off], data[off + 1]]);
        let p = u16::from_le_bytes([data[off + 2], data[off + 3]]);
        if p == pid {
            let mut val = [0u8; 8];
            val.copy_from_slice(&data[off + 8..off + 16]);
            return Some((typ, val));
        }
        off += 16;
    }
    None
}

fn read_u32_prop(cfb: &mut Cfb<'_>, prefix: &str, pid: u16, header: usize) -> Option<u32> {
    let (_, v) = read_fixed_prop(cfb, prefix, pid, header)?;
    Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

/// FILETIME (100 ns since 1601-01-01) → "YYYY-MM-DD HH:MM:SS" (UTC).
/// Converts to Unix time, then uses Howard Hinnant's civil-from-days algorithm.
fn filetime_to_iso(ft: u64) -> Option<String> {
    if ft == 0 {
        return None;
    }
    // Seconds between 1601-01-01 and 1970-01-01.
    const EPOCH_DIFF: i64 = 11_644_473_600;
    let unix = (ft / 10_000_000) as i64 - EPOCH_DIFF;
    if unix < 0 {
        return None;
    }
    let days = unix.div_euclid(86_400);
    let tod = unix.rem_euclid(86_400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // Civil date from days since 1970-01-01 (719468 = days 0000-03-01 → 1970-01-01).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    Some(format!("{year:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
}

fn read_date(cfb: &mut Cfb<'_>, pid: u16) -> Option<String> {
    let (_, v) = read_fixed_prop(cfb, "", pid, 32)?;
    filetime_to_iso(u64::from_le_bytes(v))
}

/// Read a PT_SYSTIME property (8-byte FILETIME) → ISO string.
fn read_systime(cfb: &mut Cfb<'_>, pid: u16) -> Option<String> {
    let (_, v) = read_fixed_prop(cfb, "", pid, 32)?;
    filetime_to_iso(u64::from_le_bytes(v))
}

/// Read a PT_DOUBLE property (8-byte little-endian f64).
fn read_double(cfb: &mut Cfb<'_>, pid: u16) -> Option<f64> {
    let (_, v) = read_fixed_prop(cfb, "", pid, 32)?;
    Some(f64::from_le_bytes(v))
}

fn resolve_codepage(cfb: &mut Cfb<'_>) -> u32 {
    // Prefer PidTagMessageCodepage (3FFD); fall back to PidTagInternetCodepage
    // (3FDE). A 65001 (UTF-8) value here is handled safely in `decode_ansi`
    // (which validates and falls back to cp1252 for single-byte 001E streams).
    read_u32_prop(cfb, "", PID_CODEPAGE, 32)
        .or_else(|| read_u32_prop(cfb, "", PID_INTERNET_CODEPAGE, 32))
        .unwrap_or(1252)
}

// ── Sub-storage enumeration (recipients / attachments) ────────────────────────

/// Names of immediate child storages of `parent` (root = "/") matching `prefix`.
fn child_storages(cfb: &mut Cfb<'_>, parent: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = cfb.read_storage(parent) {
        for e in entries {
            if e.is_storage() {
                let name = e.name().to_string();
                if name.starts_with(prefix) {
                    out.push(name);
                }
            }
        }
    }
    out
}

fn recipient_type_label(t: u32) -> &'static str {
    match t {
        1 => "to",
        2 => "cc",
        3 => "bcc",
        _ => "to",
    }
}

/// An address worth showing a reader, or `None`.
///
/// Outlook stores an Exchange sender as an X.500 legacyDN, and formatting it
/// like an email produced output such as
/// `Ashley, Carl E (PACE) </O=GOV+DOS/OU=PUBAFFF/CN=RECIPIENTS/CN=HO/CN=ASHLEYCE2>`
/// — noise that is not routable and not what the sender's address is. The
/// display name alone is the honest answer when no SMTP address is recorded.
///
/// `addr_type` is authoritative when present (`"SMTP"` vs `"EX"`); the shape
/// check is the fallback, because plenty of files omit it.
fn usable_email(addr_type: Option<&str>, value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(t) = addr_type {
        if !t.trim().eq_ignore_ascii_case("SMTP") {
            return None;
        }
    }
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("/O=") || upper.contains("/CN=RECIPIENTS/") || upper.starts_with("EX:") {
        return None;
    }
    Some(trimmed.to_string())
}

fn read_recipients(cfb: &mut Cfb<'_>, codepage: u32, doc: &mut MsgDocument) {
    let storages = child_storages(cfb, "/", "__recip_version1.0");
    for st in storages {
        let prefix = format!("/{st}");
        let name = read_string(cfb, &prefix, PID_RECIP_NAME, codepage);
        // Same rule as the sender: an Exchange DN is not an address. (#47)
        let addr_type = read_string(cfb, &prefix, PID_RECIP_ADDRTYPE, codepage);
        let email = usable_email(None, read_string(cfb, &prefix, PID_RECIP_SMTP, codepage))
            .or_else(|| {
                usable_email(
                    addr_type.as_deref(),
                    read_string(cfb, &prefix, PID_RECIP_EMAIL, codepage),
                )
            });
        let display = match (name, email) {
            (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
            (Some(n), _) => n,
            (None, Some(e)) => e,
            (None, None) => continue,
        };
        let rtype = read_u32_prop(cfb, &prefix, PID_RECIP_TYPE, 24).unwrap_or(1);
        match recipient_type_label(rtype) {
            "cc" => doc.cc.push(display),
            "bcc" => doc.bcc.push(display),
            _ => doc.to.push(display),
        }
    }
}

/// Extension for an image attachment, from its MIME type or its filename.
///
/// `.msg` records both, and neither is reliable on its own: Outlook often
/// stores `application/octet-stream` for a perfectly good JPEG, and plenty of
/// attachments have no filename at all.
fn image_ext_for(mime: Option<&str>, filename: Option<&str>) -> Option<&'static str> {
    if let Some(name) = filename {
        let lower = name.to_ascii_lowercase();
        for ext in [".png", ".jpeg", ".jpg", ".gif", ".webp", ".bmp", ".tiff", ".tif"] {
            if lower.ends_with(ext) {
                return Some(match ext {
                    ".jpg" => ".jpg",
                    ".tif" => ".tif",
                    e => e,
                });
            }
        }
    }
    let m = mime?.to_ascii_lowercase();
    let m = m.split(';').next()?.trim().to_string();
    match m.as_str() {
        "image/png" => Some(".png"),
        "image/jpeg" | "image/jpg" => Some(".jpg"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        "image/bmp" | "image/x-ms-bmp" => Some(".bmp"),
        "image/tiff" => Some(".tiff"),
        _ => None,
    }
}

/// Recognise an image from its magic bytes.
///
/// The last line of defence for the `application/octet-stream` + no-extension
/// case, which is common in real Outlook mail.
fn sniff_image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(".png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(".jpg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(".gif");
    }
    if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(".webp");
    }
    if bytes.starts_with(b"BM") {
        return Some(".bmp");
    }
    if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return Some(".tiff");
    }
    None
}

fn read_attachments(cfb: &mut Cfb<'_>, codepage: u32, depth: usize, doc: &mut MsgDocument) {
    if depth > 3 {
        return; // guard against pathological nesting
    }
    let storages = child_storages(cfb, "/", "__attach_version1.0");
    for st in storages {
        let prefix = format!("/{st}");
        let mut att = Attachment {
            filename: read_string(cfb, &prefix, PID_ATTACH_LONG_FILENAME, codepage)
                .or_else(|| read_string(cfb, &prefix, PID_ATTACH_FILENAME, codepage)),
            mime: read_string(cfb, &prefix, PID_ATTACH_MIME, codepage),
            size: read_u32_prop(cfb, &prefix, PID_ATTACH_SIZE, 24).map(|s| s as u64),
            embedded_text: None,
        };
        // Embedded message (attach method 5): the data property is a sub-storage
        // holding a full nested message → recurse and inline its text.
        let method = read_u32_prop(cfb, &prefix, PID_ATTACH_METHOD, 24).unwrap_or(0);
        if method == 5 {
            let embed_prefix = format!("{prefix}/__substg1.0_{PID_ATTACH_DATA:04X}000D");
            if let Some(sub) = extract_embedded(cfb, &embed_prefix, depth + 1) {
                let mut parts = Vec::new();
                if let Some(s) = &sub.subject {
                    parts.push(s.clone());
                }
                if let Some(b) = &sub.body {
                    parts.push(b.clone());
                }
                let text = parts.join("\n\n");
                if !text.trim().is_empty() {
                    att.embedded_text = Some(text);
                }
                if att.filename.is_none() {
                    att.filename = sub.subject.clone().map(|s| format!("{s}.msg"));
                }
            }
        }
        // Non-embedded attachments: read the bytes so image attachments can be
        // returned by `list_images`, which was a no-op for .msg while .eml has
        // always supported it. tika_test-outlook2003.msg carries 11 JPEGs. (#48)
        if method != 5 {
            if let Some(bytes) = read_binary(cfb, &prefix, PID_ATTACH_DATA) {
                if !bytes.is_empty() {
                    if let Some(ext) = image_ext_for(att.mime.as_deref(), att.filename.as_deref())
                        .or_else(|| sniff_image_ext(&bytes))
                    {
                        let name = match att.filename.as_deref() {
                            Some(f) if !f.trim().is_empty() => f.to_string(),
                            _ => {
                                format!("image_{}{ext}", doc.images.len() + 1)
                            }
                        };
                        doc.images.push((name, bytes));
                    }
                }
            }
        }
        doc.attachments.push(att);
    }
}

/// Minimal recursive extraction of an embedded message sub-storage (subject +
/// body only — enough to inline attachment content).
fn extract_embedded(cfb: &mut Cfb<'_>, prefix: &str, depth: usize) -> Option<MsgDocument> {
    if depth > 3 {
        return None;
    }
    let codepage = read_u32_prop(cfb, prefix, PID_CODEPAGE, 24).unwrap_or(1252);
    let subject = read_string(cfb, prefix, PID_SUBJECT, codepage);
    let body = read_body(cfb, prefix, codepage);
    if subject.is_none() && body.is_none() {
        return None;
    }
    Some(MsgDocument {
        subject,
        body,
        ..Default::default()
    })
}

// ── Body (HTML → plain → RTF) ─────────────────────────────────────────────────

/// Drop HTML tags and collapse whitespace (lightweight HTML → text).
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read_body(cfb: &mut Cfb<'_>, prefix: &str, codepage: u32) -> Option<String> {
    // 1) Plain body — cleanest for chunking, present in almost all messages.
    if let Some(b) = read_string(cfb, prefix, PID_BODY, codepage) {
        if !b.trim().is_empty() {
            return Some(b);
        }
    }
    // 2) HTML body → text.
    if let Some(bytes) = read_binary(cfb, prefix, PID_HTML) {
        let (html, _, _) = encoding_for_codepage(codepage).decode(&bytes);
        let text = html_to_text(&html);
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    if let Some(html) = read_string(cfb, prefix, PID_HTML, codepage) {
        let text = html_to_text(&html);
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    // 3) Compressed RTF → text (LZFu, then the engine's own RTF reader).
    if let Some(bytes) = read_binary(cfb, prefix, PID_RTF_COMPRESSED) {
        if let Some(text) = compressed_rtf_to_text(&bytes) {
            return Some(text);
        }
    }
    None
}

fn importance_label(v: u32) -> &'static str {
    match v {
        0 => "Low",
        2 => "High",
        _ => "Normal",
    }
}

fn task_status_label(v: u32) -> &'static str {
    match v {
        1 => "In Progress",
        2 => "Complete",
        3 => "Waiting on someone else",
        4 => "Deferred",
        _ => "Not Started",
    }
}

/// Item-type-specific fields, resolved via the named-property map for
/// appointments/tasks and standard `0x3A**` properties for contacts.
fn read_item_fields(cfb: &mut Cfb<'_>, nameid: &super::nameid::NameIdMap, codepage: u32, doc: &mut MsgDocument) {
    use super::nameid::{PSETID_APPOINTMENT, PSETID_TASK};
    let class = doc.message_class.to_ascii_lowercase();

    if class.starts_with("ipm.appointment") || class.starts_with("ipm.schedule") {
        if let Some(pid) = nameid.lid(&PSETID_APPOINTMENT, 0x820D) {
            if let Some(d) = read_systime(cfb, pid) {
                doc.item_fields.push(("Start".to_string(), d));
            }
        }
        if let Some(pid) = nameid.lid(&PSETID_APPOINTMENT, 0x820E) {
            if let Some(d) = read_systime(cfb, pid) {
                doc.item_fields.push(("End".to_string(), d));
            }
        }
        if let Some(pid) = nameid.lid(&PSETID_APPOINTMENT, 0x8208) {
            if let Some(loc) = read_string(cfb, "", pid, codepage) {
                doc.item_fields.push(("Location".to_string(), loc));
            }
        }
    } else if class.starts_with("ipm.task") {
        if let Some(pid) = nameid.lid(&PSETID_TASK, 0x8104) {
            if let Some(d) = read_systime(cfb, pid) {
                doc.item_fields.push(("Task Start".to_string(), d));
            }
        }
        if let Some(pid) = nameid.lid(&PSETID_TASK, 0x8105) {
            if let Some(d) = read_systime(cfb, pid) {
                doc.item_fields.push(("Due".to_string(), d));
            }
        }
        if let Some(pid) = nameid.lid(&PSETID_TASK, 0x8101) {
            if let Some(v) = read_u32_prop(cfb, "", pid, 32) {
                doc.item_fields
                    .push(("Status".to_string(), task_status_label(v).to_string()));
            }
        }
        if let Some(pid) = nameid.lid(&PSETID_TASK, 0x8102) {
            if let Some(p) = read_double(cfb, pid) {
                doc.item_fields
                    .push(("% Complete".to_string(), format!("{}", (p * 100.0).round() as i64)));
            }
        }
    } else if class.starts_with("ipm.contact") {
        // Contact card fields are standard 0x3A** string properties.
        for (pid, label) in [
            (0x3A16u16, "Company"),
            (0x3A17, "Job Title"),
            (0x3A18, "Department"),
            (0x3A08, "Business Phone"),
            (0x3A1C, "Mobile Phone"),
            (0x3A1B, "Home Phone"),
            (0x3A00, "Account"),
        ] {
            if let Some(v) = read_string(cfb, "", pid, codepage) {
                doc.item_fields.push((label.to_string(), v));
            }
        }
    }
}

// ── Top-level extraction ──────────────────────────────────────────────────────

pub fn extract_document(file_path: &str) -> Result<MsgDocument, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read .msg file: {e}"))?;
    extract_document_bytes(&bytes)
}

pub fn extract_document_bytes(bytes: &[u8]) -> Result<MsgDocument, String> {
    // Borrowed cursor: the CFB is read-only here, so no copy of the file bytes.
    let mut cfb = cfb::CompoundFile::open(std::io::Cursor::new(bytes))
        .map_err(|e| format!("Not a valid .msg (CFB) file: {e}"))?;

    // Sanity: must look like a MAPI message store.
    if cfb.open_stream("/__properties_version1.0").is_err()
        && read_string(&mut cfb, "", PID_BODY, 1252).is_none()
        && read_string(&mut cfb, "", PID_SUBJECT, 1252).is_none()
    {
        return Err("Not an Outlook .msg file (no MAPI property streams found)".to_string());
    }

    let codepage = resolve_codepage(&mut cfb);
    let mut doc = MsgDocument {
        message_class: read_string(&mut cfb, "", PID_MESSAGE_CLASS, codepage)
            .unwrap_or_else(|| "IPM.Note".to_string()),
        subject: read_string(&mut cfb, "", PID_SUBJECT, codepage),
        conversation_topic: read_string(&mut cfb, "", PID_CONVERSATION_TOPIC, codepage),
        sent_date: read_date(&mut cfb, PID_CLIENT_SUBMIT_TIME),
        received_date: read_date(&mut cfb, PID_MESSAGE_DELIVERY_TIME),
        importance: read_u32_prop(&mut cfb, "", PID_IMPORTANCE, 32)
            .map(|v| importance_label(v).to_string()),
        body: read_body(&mut cfb, "", codepage),
        ..Default::default()
    };

    // From (name + best-available email).
    let from_name = read_string(&mut cfb, "", PID_SENDER_NAME, codepage);
    // PidTagSenderSmtpAddress is SMTP by definition; PidTagSenderEmailAddress
    // is only an address when the addr-type says so. (#47)
    let sender_addr_type = read_string(&mut cfb, "", PID_SENDER_ADDRTYPE, codepage);
    let from_email = usable_email(None, read_string(&mut cfb, "", PID_SENDER_SMTP, codepage))
        .or_else(|| {
            usable_email(
                sender_addr_type.as_deref(),
                read_string(&mut cfb, "", PID_SENDER_EMAIL, codepage),
            )
        });
    doc.from = match (from_name, from_email) {
        (Some(n), Some(e)) if n != e => Some(format!("{n} <{e}>")),
        (Some(n), _) => Some(n),
        (None, Some(e)) => Some(e),
        (None, None) => None,
    };

    // Recipients: prefer the real recipient table; fall back to display strings.
    read_recipients(&mut cfb, codepage, &mut doc);
    if doc.to.is_empty() {
        if let Some(to) = read_string(&mut cfb, "", PID_DISPLAY_TO, codepage) {
            doc.to.push(to);
        }
    }
    if doc.cc.is_empty() {
        if let Some(cc) = read_string(&mut cfb, "", PID_DISPLAY_CC, codepage) {
            doc.cc.push(cc);
        }
    }
    if doc.bcc.is_empty() {
        if let Some(bcc) = read_string(&mut cfb, "", PID_DISPLAY_BCC, codepage) {
            doc.bcc.push(bcc);
        }
    }

    // Item-type-specific fields (appointment/contact/task) via the named-property map.
    let nameid = super::nameid::parse(&mut cfb);
    read_item_fields(&mut cfb, &nameid, codepage, &mut doc);

    read_attachments(&mut cfb, codepage, 0, &mut doc);

    Ok(doc)
}

/// Assemble the extracted document into markdown for the Markdown chunker.
pub fn document_to_markdown(doc: &MsgDocument) -> String {
    let mut out = String::new();
    if let Some(subject) = &doc.subject {
        out.push_str(&format!("# {subject}\n\n"));
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
    if let Some(d) = &doc.sent_date {
        hdr.push(format!("**Sent:** {d}"));
    }
    if let Some(imp) = &doc.importance {
        if imp != "Normal" {
            hdr.push(format!("**Importance:** {imp}"));
        }
    }
    for (label, value) in &doc.item_fields {
        hdr.push(format!("**{label}:** {value}"));
    }
    if !hdr.is_empty() {
        out.push_str(&hdr.join("  \n"));
        out.push_str("\n\n");
    }

    if let Some(body) = &doc.body {
        out.push_str(body);
        out.push_str("\n\n");
    }

    let named: Vec<&Attachment> = doc.attachments.iter().collect();
    if !named.is_empty() {
        out.push_str("## Attachments\n\n");
        for att in named {
            let name = att.filename.clone().unwrap_or_else(|| "(unnamed)".to_string());
            let mime = att
                .mime
                .as_ref()
                .map(|m| format!(" ({m})"))
                .unwrap_or_default();
            out.push_str(&format!("- {name}{mime}\n"));
            if let Some(text) = &att.embedded_text {
                out.push_str(&format!("\n{text}\n"));
            }
        }
    }

    out.trim().to_string()
}
