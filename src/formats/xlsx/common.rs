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
pub const SPREADSHEET_EXTS: &[&str] = &[
    ".xlsx", ".xls", ".xlsm", ".xlsb", ".ods", ".xltx", ".xltm",
];

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
            "Failed to open workbook: malformed or unsupported spreadsheet (parser panic)".to_string(),
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
        "xlsx" | "xlsm" | "xltx" | "xltm" => {
            Xlsx::new(cursor()).err().map(calamine::Error::Xlsx)
        }
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
        Data::Float(f) => {
            if f.fract() == 0.0 {
                format!("{}", *f as i64)
            } else {
                format!("{:.4}", f)
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string()
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
        Data::Error(_) => String::new(),
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

pub fn col_letter_to_index(col: &str) -> usize {
    col.chars().fold(0usize, |acc, c| {
        acc * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1)
    }) - 1
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
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn read_zip_entry(archive: &mut ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> Result<Option<Vec<u8>>, String> {
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

fn attr_value(attr: &quick_xml::events::attributes::Attribute<'_>) -> String {
    String::from_utf8_lossy(attr.value.as_ref()).into_owned()
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
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"Relationship" {
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
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"table" {
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
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(None)
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
    let xml_rels = format!("xl/worksheets/_rels/sheet{}.xml.rels", sheet_index_1based);
    let bin_rels = format!("xl/worksheets/_rels/sheet{}.bin.rels", sheet_index_1based);
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
fn parse_ods_named_ranges_for_sheet(content: &[u8], sheet_name: &str) -> Vec<String> {
    let mut reader = XmlReader::from_reader(content);
    let mut buf = Vec::new();
    let mut names = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name(e.name()).as_slice() == b"named-range" {
                    let mut name: Option<String> = None;
                    let mut range_sheet: Option<String> = None;
                    for attr in e.attributes().flatten() {
                        let key = local_name(QName(attr.key.as_ref()));
                        let value = attr_value(&attr);
                        match key.as_slice() {
                            b"name" => name = Some(value),
                            b"cell-range-address" | b"base-cell-address"
                                if range_sheet.is_none() =>
                            {
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
        let mut start_row_index = header_row_index.map_or(0, |idx| idx + 1);
        let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if col_count == 0 {
            continue;
        }
        // F2 guard: if every row was consumed as the header (no data rows follow,
        // e.g. a single merged title cell), fall back to emitting the header row
        // as content rather than silently dropping the whole sheet.
        let has_data_rows = rows
            .iter()
            .skip(start_row_index)
            .any(|row| !(skip_empty_rows && row_is_empty(row)));
        if !has_data_rows {
            if let Some(hidx) = header_row_index {
                start_row_index = hidx;
            }
        }
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
