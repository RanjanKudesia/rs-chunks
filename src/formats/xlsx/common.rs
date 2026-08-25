use calamine::{Data, Reader};
use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader as XmlReader;
use serde_json::{json, Value};
use std::io::Read;
use zip::ZipArchive;

pub const CT_ROW: &str = "row_document";
pub const CT_TABLE: &str = "table_region";
pub const CT_SHEET: &str = "sheet";
pub const CT_SLIDING_WINDOW: &str = "row_window";
pub const CT_PAGE_AWARE: &str = "sheet_region";
pub const CT_SEMANTIC: &str = "semantic_group";

/// Every spreadsheet extension routed through the calamine-backed xlsx chunkers.
/// Adding a new calamine-readable format is a one-line change here.
pub const SPREADSHEET_EXTS: &[&str] =
    &[".xlsx", ".xls", ".xlsm", ".xlsb", ".ods", ".xltx", ".xltm"];

thread_local! {
    /// Set while we are deliberately catching a calamine panic, so the custom
    /// panic hook stays quiet for it without silencing panics elsewhere.
    static SUPPRESS_PANIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install (once) a panic hook that suppresses stderr output only for panics
/// raised on a thread while [`SUPPRESS_PANIC`] is set — i.e. the calamine panics
/// we already catch and convert to clean errors. Panics anywhere else print as
/// normal, preserving debuggability for the rest of the crate.
fn install_quiet_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let suppress = SUPPRESS_PANIC.with(|c| c.get());
            if !suppress {
                default_hook(info);
            }
        }));
    });
}

/// Run `f` under `catch_unwind` with calamine's panic output suppressed.
fn catch_calamine_panic<T>(f: impl FnOnce() -> T) -> Result<T, ()> {
    install_quiet_panic_hook();
    SUPPRESS_PANIC.with(|c| c.set(true));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    SUPPRESS_PANIC.with(|c| c.set(false));
    result.map_err(|_| ())
}

/// True if `path` ends in any supported spreadsheet extension (case-insensitive).
pub fn is_supported_spreadsheet(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    SPREADSHEET_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Human-readable list for error messages, e.g. ".xlsx, .xls, .xlsm, …".
pub fn supported_spreadsheet_exts_display() -> String {
    SPREADSHEET_EXTS.join(", ")
}

/// Open a spreadsheet with calamine.
///
/// `.xltx`/`.xltm` are OOXML templates that calamine's `open_workbook_auto` does
/// not recognise by extension (it would fall back to a probe that tries the Xls
/// reader first). They are byte-compatible with the Xlsx reader, so we route them
/// there explicitly — avoiding the wasted probe and making intent clear. Every
/// other extension goes through `open_workbook_auto`, which dispatches by extension
/// (xls/xlsx/xlsm/xlsb/ods) exactly as before.
/// The ODF spec makes the package `mimetype` entry OPTIONAL, but calamine's ODS
/// reader hard-rejects archives that lack it (`Ods error: 'mimetype' file not
/// found`). Some real, valid `.ods` files omit it (LibreOffice opens them fine).
/// When we detect a mimetype-less `.ods`, we repair a copy in memory — prepending
/// a stored `mimetype` entry as the spec recommends — write it to a temp `.ods`,
/// and let calamine open that instead. Returns `Some(temp_path)` when repaired.
///
/// On unix the temp file is unlinked immediately after calamine opens it; the
/// open file handle inside the returned workbook keeps the inode alive.
/// Rewrite an `.ods` zip (in memory) to prepend a `mimetype` entry when it is
/// missing, so calamine recognizes it. Returns `None` when no repair is needed.
fn repair_ods_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut archive = ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    if archive.by_name("mimetype").is_ok() {
        return None; // Has mimetype — nothing to repair.
    }
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut out);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("mimetype", stored).ok()?;
        writer
            .write_all(b"application/vnd.oasis.opendocument.spreadsheet")
            .ok()?;
        for i in 0..archive.len() {
            let file = archive.by_index_raw(i).ok()?;
            writer.raw_copy_file(file).ok()?;
        }
        writer.finish().ok()?;
    }
    Some(out.into_inner())
}

type Workbook = calamine::Sheets<std::io::Cursor<Vec<u8>>>;

pub fn open_spreadsheet(file_path: &str) -> Result<Workbook, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read spreadsheet: {e}"))?;
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    open_spreadsheet_from_bytes(&bytes, &ext)
}

/// No-filesystem workbook open (wasm/browser). calamine auto-detects the format
/// from content; `.ods` files missing their `mimetype` entry are repaired
/// in-memory first. calamine can panic on some malformed inputs — caught here.
pub fn open_spreadsheet_from_bytes(data: &[u8], ext: &str) -> Result<Workbook, String> {
    let result = catch_calamine_panic({
        let owned = repaired_bytes(data, ext);
        move || {
            calamine::open_workbook_auto_from_rs(std::io::Cursor::new(owned))
                .map_err(|e| format!("Failed to open workbook: {e}"))
        }
    });
    match result {
        Ok(Ok(wb)) => Ok(wb),
        // Recomputed rather than cloned up front: the repair is pure, and the
        // success path must not pay a full copy of the workbook for a fallback
        // that only ever runs when the open already failed.
        Ok(Err(generic)) => {
            Err(specific_open_error(&repaired_bytes(data, ext), ext).unwrap_or(generic))
        }
        Err(_) => Err(
            "Failed to open workbook: malformed or unsupported spreadsheet (parser panic)"
                .to_string(),
        ),
    }
}

/// Apply the format-specific in-memory repairs calamine needs before it will
/// open the package. Pure: same input, same output.
fn repaired_bytes(data: &[u8], ext: &str) -> Vec<u8> {
    if ext == "ods" {
        repair_ods_bytes(data).unwrap_or_else(|| data.to_vec())
    } else {
        // An XLM macro sheet or a <sheet> with an empty r:id makes calamine
        // reject the whole workbook at open time, before any sheet is read —
        // so the per-sheet isolation below never gets a chance. Drop those
        // entries from the sheet list and the ordinary worksheets load. (#21)
        super::repair::repair_ooxml_workbook_bytes(data).unwrap_or_else(|| data.to_vec())
    }
}

/// Recover the *reason* a workbook would not open, when content sniffing could
/// not even classify it.
///
/// An encrypted OOXML package is an OLE container with none of the parts
/// `open_workbook_auto_from_rs` looks for, so it reports the useless "Cannot
/// detect file format" instead of "Workbook is password protected". Opening
/// from a *path* never had this problem — calamine dispatched on the file
/// extension and the format's own reader produced the real message. We know the
/// extension here too, so ask that reader directly and keep the message.
///
/// Only ever called on the failure path, and only to improve the text: a format
/// that opens fine never reaches it.
fn specific_open_error(data: &[u8], ext: &str) -> Option<String> {
    use calamine::{Ods, Xls, Xlsb, Xlsx};
    let cursor = || std::io::Cursor::new(data.to_vec());
    // Wrap in calamine's own `Error` so the text matches what the path-based
    // `open_workbook_auto` produced ("Xlsx error: …", not bare "…").
    let err = match ext {
        "xlsx" | "xlsm" | "xltx" | "xltm" => Xlsx::new(cursor()).err().map(calamine::Error::Xlsx),
        "xlsb" => Xlsb::new(cursor()).err().map(calamine::Error::Xlsb),
        "ods" => Ods::new(cursor()).err().map(calamine::Error::Ods),
        "xls" => Xls::new(cursor()).err().map(calamine::Error::Xls),
        _ => None,
    }?;
    Some(format!("Failed to open workbook: {err}"))
}

/// Read a single worksheet range, converting both errors and calamine panics
/// into a clean `Err(String)`. See [`open_spreadsheet`] for why panics happen.
pub fn read_worksheet_range(
    workbook: &mut Workbook,
    sheet_name: &str,
) -> Result<calamine::Range<Data>, String> {
    match catch_calamine_panic(|| workbook.worksheet_range(sheet_name)) {
        Ok(Ok(range)) => Ok(range),
        Ok(Err(e)) => Err(format!("Failed to read sheet '{sheet_name}': {e}")),
        Err(_) => Err(format!(
            "Failed to read sheet '{sheet_name}': malformed or unsupported spreadsheet data (parser panic)"
        )),
    }
}

#[derive(Debug, Clone)]
pub struct XlsxChunkRecord {
    pub content: String,
    pub content_type: String,
    pub metadata: Value,
}

/// Split a flat list of serialised row strings into char-limited groups.
/// A single line that already exceeds the limit is kept as its own group rather than
/// being silently truncated.
pub fn split_content_lines(lines: Vec<String>, max_chunk_chars: usize) -> Vec<Vec<String>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_len = 0usize;

    for line in lines {
        let sep = if current.is_empty() { 0 } else { 1 };
        if !current.is_empty() && current_len + sep + line.len() > max_chunk_chars {
            parts.push(std::mem::take(&mut current));
            current_len = line.len();
            current.push(line);
        } else {
            current_len += sep + line.len();
            current.push(line);
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

pub fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::String(s) => s.clone(),
        // Excel keeps 15 significant digits and no more; anything past that in
        // the file is IEEE-754 noise from whatever wrote it (`899.20000000000073`
        // is what is stored, `899.20` is what Excel shows). So: round to 15
        // significant digits, then print the shortest decimal that round-trips.
        //
        // `{:.4}` did that noise suppression by accident and charged three ways
        // for it. It picks decimal PLACES where the contract is significant
        // DIGITS, and places are magnitude-dependent:
        //   - `3.5E-4` came out `0.0003` — a 14% error
        //     (poi_NumberFormatTests.xlsx)
        //   - anything below 5e-5 collapsed to the literal string "0", so a
        //     satoshi, FX-rate or concentration column rendered as all zeros
        //   - `as i64` SATURATES in Rust, so any integral value past i64::MAX
        //     rendered as 9223372036854775807 — Avogadro's number became that
        // The last two are unexercised by this corpus, so only unit tests can
        // pin them; the snapshot never will.
        Data::Float(f) => {
            let f = *f;
            if !f.is_finite() {
                String::new()
            } else if f.fract() == 0.0 && (f as i64) as f64 == f {
                // A round-trip guard, not a magnitude guard: take the integer
                // form only when i64 represents this value exactly, which makes
                // the saturating cast unreachable.
                format!("{}", f as i64)
            } else {
                let snapped: f64 = format!("{f:.14e}").parse().unwrap_or(f);
                format!("{snapped}")
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        // Typed date/time cells (common in .ods and .xlsb, also present in .xlsx).
        // Without this they serialised to an empty string, silently dropping whole
        // date columns. `as_datetime()` (calamine `dates` feature) yields a chrono
        // NaiveDateTime whose Display is "YYYY-MM-DD HH:MM:SS".
        Data::DateTime(dt) => dt
            .as_datetime()
            .map(|d| d.to_string())
            .unwrap_or_else(|| dt.as_f64().to_string()),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        // A broken cell is not an empty cell. `#REF!` in a chunk is
        // information; a blank is a lie about what the sheet contains.
        // poi_46535.xlsx carries 331 of these.
        Data::Error(e) => format!("{e}"),
        // Data::Empty and any future calamine variant → empty cell.
        _ => String::new(),
    }
}

pub fn detect_header_row(rows: &[&[Data]]) -> Option<usize> {
    for (i, row) in rows.iter().enumerate() {
        let non_empty: Vec<_> = row.iter().filter(|c| !matches!(c, Data::Empty)).collect();
        if non_empty.is_empty() {
            continue;
        }
        if non_empty.iter().all(|c| matches!(c, Data::String(_))) {
            return Some(i);
        }
        // Numeric index column (e.g. 0, 1, 2…) followed by all-string labels → treat as header
        if non_empty.len() >= 2
            && matches!(non_empty[0], Data::Float(_) | Data::Int(_))
            && non_empty[1..].iter().all(|c| matches!(c, Data::String(_)))
        {
            return Some(i);
        }
    }
    None
}

/// Excel's own grid limits. A `ref=` attribute or `_xlnm.Print_Area` string is
/// attacker-controlled, and both feed per-row allocations and row loops, so a
/// reference beyond the real grid is rejected rather than honoured:
/// `ref="A1:AAAAAAAAAA1"` otherwise yields a column count around 1.4e14.
pub const MAX_SHEET_COLS: usize = 16_384; // XFD
pub const MAX_SHEET_ROWS: usize = 1_048_576;

/// Convert a column label (`A`, `AB`, `XFD`) to a 0-based index.
///
/// Saturating and underflow-free on purpose: the old body was
/// `fold(...) - 1`, which underflowed to `usize::MAX` for an empty label and
/// overflowed `acc * 26` for a long run of letters — both reachable from a
/// crafted `ref=`.
pub fn col_letter_to_index(col: &str) -> usize {
    let mut acc = 0usize;
    for c in col.chars() {
        if !c.is_ascii_alphabetic() {
            break;
        }
        acc = acc
            .saturating_mul(26)
            .saturating_add(c.to_ascii_uppercase() as usize - 'A' as usize + 1);
        if acc > MAX_SHEET_COLS {
            // One past the last real column, so `parse_range_ref` rejects the
            // reference instead of silently accepting a clamped one. Saturating
            // to XFD would make `A1:AAAAAAAAAA1` look like a legal range.
            return MAX_SHEET_COLS;
        }
    }
    acc.saturating_sub(1)
}

pub fn parse_cell_ref(cell_ref: &str) -> Option<(usize, usize)> {
    let letters: String = cell_ref
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let digits: String = cell_ref
        .chars()
        .skip_while(|c| c.is_ascii_alphabetic())
        .collect();
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let col = col_letter_to_index(&letters);
    let row = digits.parse::<usize>().ok()?.saturating_sub(1);
    Some((row, col))
}

pub fn parse_range_ref(range_ref: &str) -> Option<(usize, usize, usize, usize)> {
    let parts: Vec<&str> = range_ref.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let (r1, c1) = parse_cell_ref(parts[0])?;
    let (r2, c2) = parse_cell_ref(parts[1])?;
    // Outside the real grid this is not a range, it is an allocation request.
    if r1.max(r2) >= MAX_SHEET_ROWS || c1.max(c2) >= MAX_SHEET_COLS {
        return None;
    }
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn read_zip_entry(
    archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read '{name}': {e}"))?;
            Ok(Some(buf))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("Failed to open '{name}' in xlsx archive: {e}")),
    }
}

fn local_name(name: QName<'_>) -> Vec<u8> {
    let bytes = name.as_ref();
    let idx = bytes
        .iter()
        .rposition(|b| *b == b':')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[idx..].to_vec()
}

/// Attribute values are stored escaped (`R&amp;D`, `&quot;`), so every read
/// goes through the shared entity resolver — relationship `Target`s, table and
/// named-range names, and `xlink:href` alike.
fn attr_value(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    crate::entities::decode_attr(attr)
}

fn resolve_target(base_dir: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').collect();
    for segment in target.split('/') {
        match segment {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn parse_table_relationship_targets(rels_xml: &[u8]) -> Result<Vec<String>, String> {
    let mut reader = XmlReader::from_reader(rels_xml);
    let mut buf = Vec::new();
    let mut targets = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse worksheet relationships XML: {e}")),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if local_name(e.name()).as_slice() == b"Relationship" =>
            {
                let mut rel_type = String::new();
                let mut target = String::new();
                for attr in e.attributes().flatten() {
                    let key = local_name(QName(attr.key.as_ref()));
                    if key.as_slice() == b"Type" {
                        rel_type = attr_value(&attr);
                    } else if key.as_slice() == b"Target" {
                        target = attr_value(&attr);
                    }
                }
                if rel_type.ends_with("/table") && !target.is_empty() {
                    targets.push(target);
                }
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(targets)
}

fn parse_table_name(table_xml: &[u8]) -> Result<Option<String>, String> {
    let mut reader = XmlReader::from_reader(table_xml);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Failed to parse table XML: {e}")),
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if local_name(e.name()).as_slice() == b"table" =>
            {
                let mut table_name: Option<String> = None;
                let mut display_name: Option<String> = None;
                for attr in e.attributes().flatten() {
                    let key = local_name(QName(attr.key.as_ref()));
                    let value = attr_value(&attr);
                    if key.as_slice() == b"name" {
                        table_name = Some(value);
                    } else if key.as_slice() == b"displayName" {
                        display_name = Some(value);
                    }
                }
                return Ok(table_name.or(display_name));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(None)
}

/// Resolve a worksheet's part name from `xl/workbook.xml` + its rels.
///
/// The sheet's position in the workbook is **not** its part number. OOXML lists
/// `<sheet name="X" r:id="rIdN"/>` and the rels map `rIdN` to an arbitrary
/// target, so `sheet{ordinal}.xml` is a guess. Measured on
/// `poi_xlmmacro.xlsm`, where an XLM macro sheet occupies a slot and shifts
/// every worksheet after it: ordinal 2 resolves to `sheet1.xml`, ordinal 3 to
/// `sheet2.xml`. Named tables, images and drawings were therefore read from the
/// **wrong sheet** — silently, since the wrong sheet is still a valid sheet.
///
/// Returns the part path without the `xl/` prefix (e.g. `worksheets/sheet1.xml`),
/// or `None` when the workbook or its rels cannot be read — the caller then
/// keeps the historical ordinal guess, which is correct for 60 of the 62
/// zip-backed workbooks in the corpus.
pub fn resolve_sheet_part(
    archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>,
    sheet_name: &str,
) -> Option<String> {
    let wb = read_zip_entry(archive, "xl/workbook.xml").ok()??;
    let rels = read_zip_entry(archive, "xl/_rels/workbook.xml.rels").ok()??;

    let rid = sheet_rid_for_name(&wb, sheet_name)?;
    let target = rels_target_for_id(&rels, &rid)?;
    Some(
        target
            .trim_start_matches('/')
            .trim_start_matches("xl/")
            .to_string(),
    )
}

fn sheet_rid_for_name(workbook_xml: &[u8], want: &str) -> Option<String> {
    let mut reader = XmlReader::from_reader(workbook_xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local: &[u8] = name
                    .as_ref()
                    .rsplit(|b| *b == b':')
                    .next()
                    .unwrap_or(name.as_ref());
                if local == b"sheet" {
                    let mut this_name = String::new();
                    let mut rid = String::new();
                    for attr in e.attributes().flatten() {
                        let key = attr.key.as_ref();
                        let local_key: &[u8] = key.rsplit(|b| *b == b':').next().unwrap_or(key);
                        let val = crate::entities::decode_attr(&attr);
                        match local_key {
                            b"name" => this_name = val,
                            b"id" => rid = val,
                            _ => {}
                        }
                    }
                    if this_name == want && !rid.is_empty() {
                        return Some(rid);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn rels_target_for_id(rels_xml: &[u8], want: &str) -> Option<String> {
    let mut reader = XmlReader::from_reader(rels_xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local: &[u8] = name
                    .as_ref()
                    .rsplit(|b| *b == b':')
                    .next()
                    .unwrap_or(name.as_ref());
                if local == b"Relationship" {
                    let mut id = String::new();
                    let mut target = String::new();
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = crate::entities::decode_attr(&attr);
                        match key.as_str() {
                            "Id" => id = val,
                            "Target" => target = val,
                            _ => {}
                        }
                    }
                    if id == want && !target.is_empty() {
                        return Some(target);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// The `_rels` path for a worksheet part, e.g.
/// `worksheets/sheet1.xml` -> `xl/worksheets/_rels/sheet1.xml.rels`.
pub fn sheet_rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("xl/{dir}/_rels/{file}.rels"),
        None => format!("xl/_rels/{part}.rels"),
    }
}

pub fn get_named_table_names_for_sheet(
    data: &[u8],
    ext: &str,
    sheet_index_1based: usize,
    sheet_name: &str,
) -> Result<Vec<String>, String> {
    let mut archive = match ZipArchive::new(std::io::Cursor::new(data.to_vec())) {
        Ok(a) => a,
        Err(_) => return Ok(Vec::new()), // Not a ZIP archive (e.g. XLS) — no named tables
    };

    // ODF (.ods) stores named ranges as `table:named-range` in content.xml,
    // attributed to a sheet by the `table:cell-range-address` prefix
    // ("Sheet1.$A$1"). This is the ODS analogue of OOXML named tables.
    if ext == "ods" {
        if let Some(content) = read_zip_entry(&mut archive, "content.xml")? {
            return Ok(parse_ods_named_ranges_for_sheet(&content, sheet_name));
        }
        return Ok(Vec::new());
    }

    // .xlsx/.xlsm/.xltx/.xltm → sheetN.xml.rels; .xlsb → sheetN.bin.rels.
    // The referenced table parts (xl/tables/tableN.xml) are XML in both.
    // Prefer the workbook relationship over the ordinal guess — the sheet's
    // position is not its part number (see `resolve_sheet_part`).
    let resolved = resolve_sheet_part(&mut archive, sheet_name).map(|p| sheet_rels_path(&p));
    let xml_rels = resolved
        .clone()
        .unwrap_or_else(|| format!("xl/worksheets/_rels/sheet{}.xml.rels", sheet_index_1based));
    let bin_rels = resolved
        .unwrap_or_else(|| format!("xl/worksheets/_rels/sheet{}.bin.rels", sheet_index_1based));
    let rels_xml = match read_zip_entry(&mut archive, &xml_rels)? {
        Some(b) => b,
        None => match read_zip_entry(&mut archive, &bin_rels)? {
            Some(b) => b,
            None => return Ok(Vec::new()),
        },
    };

    let targets = parse_table_relationship_targets(&rels_xml)?;
    let mut names = Vec::new();
    for target in targets {
        let full_path = resolve_target("xl/worksheets", &target);
        let Some(table_xml) = read_zip_entry(&mut archive, &full_path)? else {
            continue;
        };
        if let Some(name) = parse_table_name(&table_xml)? {
            names.push(name);
        }
    }

    Ok(names)
}

/// Parse `table:named-range` elements from an ODS `content.xml`, returning the
/// names of those attributed to `sheet_name` (via the `table:cell-range-address`
/// or `table:base-cell-address` prefix, e.g. `"Sheet1.$A$1"`).
/// One ODS named range: its name and the cell region it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdsNamedRange {
    pub name: String,
    pub start_row: usize,
    pub end_row: usize,
    pub start_col: usize,
    pub end_col: usize,
}

/// Parse an ODF cell reference — `Sheet1.$A$1`, `.$C$4`, `$Sheet1.A1` — into a
/// 0-based (row, col).
fn parse_ods_cell_ref(reference: &str) -> Option<(usize, usize)> {
    let cell = reference.rsplit('.').next()?;
    let cell = cell.trim_start_matches('$');
    let mut col = 0usize;
    let mut chars = cell.chars().peekable();
    let mut saw_letter = false;
    while let Some(c) = chars.peek() {
        if c.is_ascii_alphabetic() {
            col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            saw_letter = true;
            chars.next();
        } else {
            break;
        }
    }
    if !saw_letter {
        return None;
    }
    let rest: String = chars.collect();
    let row: usize = rest.trim_start_matches('$').parse().ok()?;
    (row > 0).then(|| (row - 1, col - 1))
}

/// Named ranges on `sheet_name`, with the region each one covers.
///
/// [`get_named_table_names_for_sheet`] returns only the names, which is all
/// `sheet` mode needs. `table` mode has to know *where* each range is to decide
/// whether a detected region is that named table (TECH_DEBT #20).
pub fn get_ods_named_ranges_for_sheet(content: &[u8], sheet_name: &str) -> Vec<OdsNamedRange> {
    let mut reader = XmlReader::from_reader(content);
    let mut buf = Vec::new();
    let mut out = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if local_name(e.name()).as_slice() == b"named-range" =>
            {
                let mut name: Option<String> = None;
                let mut address: Option<String> = None;
                for attr in e.attributes().flatten() {
                    let key = local_name(QName(attr.key.as_ref()));
                    let value = attr_value(&attr);
                    match key.as_slice() {
                        b"name" => name = Some(value),
                        b"cell-range-address" => address = Some(value),
                        _ => {}
                    }
                }
                if let (Some(name), Some(address)) = (name, address) {
                    let on_sheet = address
                        .split('.')
                        .next()
                        .map(|s| s.trim_start_matches('$').trim_matches('\''))
                        .is_some_and(|s| s == sheet_name);
                    // A single-cell range has no ':' — `Sheet1.$A$1`. It is
                    // still a named range, and its region is that one cell.
                    let (from, to) = match address.split_once(':') {
                        Some((a, b)) => (a, b),
                        None => (address.as_str(), address.as_str()),
                    };
                    if let (true, Some(a), Some(b)) =
                        (on_sheet, parse_ods_cell_ref(from), parse_ods_cell_ref(to))
                    {
                        out.push(OdsNamedRange {
                            name,
                            start_row: a.0.min(b.0),
                            end_row: a.0.max(b.0),
                            start_col: a.1.min(b.1),
                            end_col: a.1.max(b.1),
                        });
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

fn parse_ods_named_ranges_for_sheet(content: &[u8], sheet_name: &str) -> Vec<String> {
    let mut reader = XmlReader::from_reader(content);
    let mut buf = Vec::new();
    let mut names = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if local_name(e.name()).as_slice() == b"named-range" =>
            {
                let mut name: Option<String> = None;
                let mut range_sheet: Option<String> = None;
                for attr in e.attributes().flatten() {
                    let key = local_name(QName(attr.key.as_ref()));
                    let value = attr_value(&attr);
                    match key.as_slice() {
                        b"name" => name = Some(value),
                        b"cell-range-address" | b"base-cell-address" if range_sheet.is_none() => {
                            // "Sheet1.$A$1" → sheet is the part before the first '.'
                            let raw = value.split('.').next().unwrap_or("");
                            range_sheet =
                                Some(raw.trim_start_matches('$').trim_matches('\'').to_string());
                        }
                        _ => {}
                    }
                }
                if let (Some(name), Some(rs)) = (name, range_sheet) {
                    if rs == sheet_name {
                        names.push(name);
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    names
}

#[derive(Debug, Clone)]
pub struct DataRegion {
    pub start_row: usize,
    pub end_row: usize,
    pub start_col: usize,
    pub end_col: usize,
}

pub fn detect_contiguous_regions(rows: &[&[Data]], base_row: usize) -> Vec<DataRegion> {
    let mut regions = Vec::new();

    let mut current_start: Option<usize> = None;
    let mut current_min_col = usize::MAX;
    let mut current_max_col = 0usize;

    for (row_idx, row) in rows.iter().enumerate() {
        let mut first_non_empty: Option<usize> = None;
        let mut last_non_empty: Option<usize> = None;
        for (col_idx, cell) in row.iter().enumerate() {
            if !matches!(cell, Data::Empty) {
                if first_non_empty.is_none() {
                    first_non_empty = Some(col_idx);
                }
                last_non_empty = Some(col_idx);
            }
        }

        match (first_non_empty, last_non_empty) {
            (Some(first), Some(last)) => {
                if current_start.is_none() {
                    current_start = Some(row_idx);
                    current_min_col = first;
                    current_max_col = last;
                } else {
                    current_min_col = current_min_col.min(first);
                    current_max_col = current_max_col.max(last);
                }
            }
            _ => {
                if let Some(start) = current_start {
                    let end = row_idx.saturating_sub(1);
                    regions.push(DataRegion {
                        start_row: base_row + start,
                        end_row: base_row + end,
                        start_col: current_min_col,
                        end_col: current_max_col,
                    });
                    current_start = None;
                    current_min_col = usize::MAX;
                    current_max_col = 0;
                }
            }
        }
    }

    if let Some(start) = current_start {
        let end = rows.len().saturating_sub(1);
        regions.push(DataRegion {
            start_row: base_row + start,
            end_row: base_row + end,
            start_col: current_min_col,
            end_col: current_max_col,
        });
    }

    regions
}

pub fn serialize_row_kv(headers: &[String], cells: &[Data]) -> String {
    (0..headers.len())
        .map(|idx| {
            let value = cells.get(idx).map(cell_to_string).unwrap_or_default();
            format!("{}: {}", headers[idx], value)
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn row_is_empty(row: &[Data]) -> bool {
    row.iter().all(|cell| matches!(cell, Data::Empty))
}

pub fn row_is_empty_public(row: &[Data]) -> bool {
    row_is_empty(row)
}

/// Index of the first data row, honouring the header fallback.
///
/// Header detection is a heuristic. When it decides that a sheet's *only*
/// rows are headers, starting after them yields nothing and the sheet is
/// dropped without a word — losing a single-cell sheet, a merged title, or a
/// template that carries just its column names. In that case fall back to the
/// header row itself so its content is emitted.
///
/// This lives here because it applies to **every** row-consuming mode. It used
/// to be written out per mode, and only `row` and `sheet` ever got it:
/// `semantic`, `page_aware`, `sliding_window` and `table` each silently dropped
/// content on ~14 fixtures in the corpus, and batch `sliding_window` disagreed
/// with its own streaming path as a result (TECH_DEBT #80). One definition, one
/// behaviour.
///
/// Callers may pass raw or column-padded rows: padding adds `Data::Empty`,
/// which cannot change whether a row is empty.
pub fn data_start_with_header_fallback(
    rows: &[&[Data]],
    header_row_index: Option<usize>,
    skip_empty_rows: bool,
) -> usize {
    let data_start = header_row_index.map_or(0, |idx| idx + 1);
    let has_data_rows = rows
        .iter()
        .skip(data_start)
        .any(|row| !(skip_empty_rows && row_is_empty(row)));
    if has_data_rows {
        data_start
    } else {
        header_row_index.unwrap_or(data_start)
    }
}

fn build_headers(
    rows: &[&[Data]],
    header_row_index: Option<usize>,
    col_count: usize,
) -> Vec<String> {
    let mut headers = Vec::with_capacity(col_count);
    for idx in 0..col_count {
        let header = header_row_index
            .and_then(|row_index| rows.get(row_index))
            .and_then(|row| row.get(idx))
            .map(cell_to_string)
            .unwrap_or_default();
        if header.trim().is_empty() {
            headers.push(format!("Column {}", idx + 1));
        } else {
            headers.push(header);
        }
    }
    headers
}

fn serialize_row_values(cells: &[Data], col_count: usize) -> String {
    (0..col_count)
        .map(|idx| cells.get(idx).map(cell_to_string).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn serialize_row_values_public(cells: &[Data], col_count: usize) -> String {
    serialize_row_values(cells, col_count)
}

fn build_chunk_content(
    grouped_rows: &[(usize, &[Data])],
    headers: &[String],
    include_headers: bool,
    col_count: usize,
) -> String {
    grouped_rows
        .iter()
        .map(|(_, row)| {
            if include_headers {
                serialize_row_kv(headers, row)
            } else {
                serialize_row_values(row, col_count)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Record which sheets were skipped, on every chunk.
///
/// #8 made an unreadable sheet skippable instead of fatal, which was right —
/// but it made the loss *silent*: a workbook whose chart sheet cannot be read
/// now returns the other sheets and says nothing. `skipped_sheets` is always
/// present (empty when nothing was dropped) so its absence never has to be
/// interpreted. (#66)
pub fn stamp_skipped_sheets(chunks: &mut [XlsxChunkRecord], skipped: &[String]) {
    for chunk in chunks.iter_mut() {
        if let Some(map) = chunk.metadata.as_object_mut() {
            map.insert("skipped_sheets".into(), json!(skipped));
        }
    }
}

pub fn build_row_chunks(
    data: &[u8],
    ext: &str,
    rows_per_chunk: usize,
    include_headers: bool,
    sheet_names: Vec<String>,
    skip_empty_rows: bool,
) -> Result<Vec<XlsxChunkRecord>, String> {
    let mut workbook = open_spreadsheet_from_bytes(data, ext)?;

    let workbook_sheet_names = workbook.sheet_names().to_vec();
    let selected_sheets = if sheet_names.is_empty() {
        workbook_sheet_names.clone()
    } else {
        for sheet_name in &sheet_names {
            if !workbook_sheet_names.iter().any(|name| name == sheet_name) {
                return Err(format!("Sheet '{sheet_name}' not found"));
            }
        }
        sheet_names
    };

    let mut chunks = Vec::new();
    let mut readable_sheets = 0usize;
    let mut first_sheet_error: Option<String> = None;
    let mut skipped_sheets: Vec<String> = Vec::new();
    for sheet_name in selected_sheets {
        let sheet_index = workbook_sheet_names
            .iter()
            .position(|name| name == &sheet_name)
            .unwrap_or(0);

        // A sheet calamine cannot read (chart sheets, XLM macro sheets) must not
        // take the whole workbook down with it — skip it and keep going.
        let range = match read_worksheet_range(&mut workbook, &sheet_name) {
            Ok(range) => {
                readable_sheets += 1;
                range
            }
            Err(e) => {
                first_sheet_error.get_or_insert(e);
                skipped_sheets.push(sheet_name.clone());
                continue;
            }
        };
        let base_row_index = range.start().map(|(row, _)| row as usize).unwrap_or(0);

        let rows: Vec<&[Data]> = range.rows().collect();
        if rows.is_empty() {
            continue;
        }

        let header_row_index = detect_header_row(&rows);
        let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }
        let start_row_index =
            data_start_with_header_fallback(&rows, header_row_index, skip_empty_rows);
        let headers = build_headers(&rows, header_row_index, col_count);

        let mut pending_rows: Vec<(usize, &[Data])> = Vec::new();
        let mut chunk_index = 0usize;

        for (row_index, row) in rows.iter().enumerate().skip(start_row_index) {
            if skip_empty_rows && row_is_empty(row) {
                continue;
            }

            pending_rows.push((base_row_index + row_index, row));
            if pending_rows.len() == rows_per_chunk {
                let content =
                    build_chunk_content(&pending_rows, &headers, include_headers, col_count);
                let first_row_index = pending_rows[0].0;
                let actual_row_count = pending_rows.len();
                chunks.push(XlsxChunkRecord {
                    content,
                    content_type: CT_ROW.to_string(),
                    metadata: json!({
                        "sheet_name": sheet_name.clone(),
                        "sheet_index": sheet_index,
                        "row_index": first_row_index,
                        "header_row": headers.clone(),
                        "col_count": col_count,
                        "rows_per_chunk": rows_per_chunk,
                        "actual_row_count": actual_row_count,
                        "chunk_index": chunk_index,
                    }),
                });
                pending_rows.clear();
                chunk_index += 1;
            }
        }

        if !pending_rows.is_empty() {
            let content = build_chunk_content(&pending_rows, &headers, include_headers, col_count);
            let first_row_index = pending_rows[0].0;
            let actual_row_count = pending_rows.len();
            chunks.push(XlsxChunkRecord {
                content,
                content_type: CT_ROW.to_string(),
                metadata: json!({
                    "sheet_name": sheet_name.clone(),
                    "sheet_index": sheet_index,
                    "row_index": first_row_index,
                    "header_row": headers.clone(),
                    "col_count": col_count,
                    "rows_per_chunk": rows_per_chunk,
                    "actual_row_count": actual_row_count,
                    "chunk_index": chunk_index,
                }),
            });
        }
    }
    // Every selected sheet failed to read: this is not an empty workbook,
    // it is an unreadable one — surface the first failure rather than
    // returning success with no chunks.
    if readable_sheets == 0 {
        if let Some(e) = first_sheet_error {
            return Err(e);
        }
    }

    stamp_skipped_sheets(&mut chunks, &skipped_sheets);
    Ok(chunks)
}

#[cfg(test)]
mod cell_rendering_tests {
    use super::cell_to_string;
    use calamine::Data;

    /// `{:.4}` truncated to four decimal PLACES where the contract is fifteen
    /// significant DIGITS. Places are magnitude-dependent, so small values lost
    /// everything: `3.5E-4` came out `0.0003`, a 14% error, and anything below
    /// 5e-5 collapsed to the literal string "0".
    #[test]
    fn small_magnitudes_keep_their_value() {
        assert_eq!(cell_to_string(&Data::Float(3.5e-4)), "0.00035");
        assert_eq!(cell_to_string(&Data::Float(1e-8)), "0.00000001");
        assert_ne!(cell_to_string(&Data::Float(1e-8)), "0");
    }

    /// Rust's float->int `as` cast SATURATES. Any integral value past i64::MAX
    /// rendered as 9223372036854775807 — Avogadro's number became that literal.
    #[test]
    fn huge_integral_values_do_not_saturate() {
        let out = cell_to_string(&Data::Float(6.02214076e23));
        assert_ne!(out, "9223372036854775807", "the cast still saturates");
        assert!(out.starts_with("602214076"), "unexpected rendering: {out}");
        assert_ne!(
            cell_to_string(&Data::Float(1e19)),
            "9223372036854775807",
            "the cast still saturates"
        );
    }

    /// The half that must not regress: IEEE-754 noise is still suppressed, and
    /// ordinary values still render cleanly.
    #[test]
    fn ieee_noise_is_still_suppressed() {
        assert_eq!(cell_to_string(&Data::Float(0.18999999999999995)), "0.19");
        assert_eq!(cell_to_string(&Data::Float(2500.0)), "2500");
        assert_eq!(cell_to_string(&Data::Float(-0.5)), "-0.5");
        assert_eq!(cell_to_string(&Data::Float(0.0)), "0");
    }

    /// A non-finite value has no honest decimal form; an empty cell is closer
    /// to the truth than "NaN" or "inf" appearing as data.
    #[test]
    fn non_finite_values_render_empty() {
        assert_eq!(cell_to_string(&Data::Float(f64::NAN)), "");
        assert_eq!(cell_to_string(&Data::Float(f64::INFINITY)), "");
    }

    /// A broken cell is not an empty cell.
    #[test]
    fn error_cells_say_what_they_are() {
        use calamine::CellErrorType;
        assert_eq!(cell_to_string(&Data::Error(CellErrorType::Div0)), "#DIV/0!");
        assert_eq!(cell_to_string(&Data::Error(CellErrorType::Ref)), "#REF!");
        assert_ne!(cell_to_string(&Data::Error(CellErrorType::NA)), "");
    }
}
