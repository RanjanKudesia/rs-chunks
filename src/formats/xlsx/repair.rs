//! In-memory repair of OOXML workbooks calamine refuses to open.
//!
//! Sibling of `common::repair_ods_bytes` (which adds a missing `mimetype`).
//! This one deals with `<sheet>` entries in `xl/workbook.xml` that calamine
//! cannot dispatch, and which make it reject the **entire** workbook before a
//! single cell is read:
//!
//! * `poi_xlmmacro.xlsm` — a legacy XLM macro sheet, related as
//!   `.../office/2006/relationships/xlMacrosheet`. calamine reports
//!   "Unrecognized sheet:type: xl/macrosheets/sheet1.xml".
//! * `closedxml_tdf111974.xlsm` — two `<sheet>` entries carry `r:id=""` and so
//!   resolve to nothing. calamine reports "Relationship not found".
//!
//! In both files the ordinary worksheets are perfectly readable; they were
//! simply unreachable. Dropping the undispatchable entries from the sheet list
//! — and nothing else — makes the rest load.
//!
//! This is deliberately *not* per-sheet error isolation (see the sheet loops in
//! `common.rs` for that). The failure here happens at `open_workbook_auto`,
//! before any sheet is read, so it has to be fixed in the package.

use std::io::Write;
use zip::ZipArchive;

const WORKBOOK: &str = "xl/workbook.xml";
const WORKBOOK_RELS: &str = "xl/_rels/workbook.xml.rels";
/// The only relationship type calamine can turn into a readable sheet.
const WORKSHEET_REL_SUFFIX: &str = "/worksheet";

/// Rewrite a workbook so it only lists sheets calamine can open.
///
/// Returns `None` when there is nothing to repair, when the package cannot be
/// read, or when *every* sheet would be dropped — an empty workbook is a worse
/// answer than the original error, so in that case the caller should let
/// calamine fail and report why.
pub fn repair_ooxml_workbook_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut archive = ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let workbook = read_entry(&mut archive, WORKBOOK)?;
    let rels = read_entry(&mut archive, WORKBOOK_RELS).unwrap_or_default();

    let worksheet_ids = worksheet_rel_ids(&rels);
    let (patched, dropped, kept) = strip_unopenable_sheets(&workbook, &worksheet_ids);
    if dropped == 0 || kept == 0 {
        return None;
    }

    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut out);
        for i in 0..archive.len() {
            let file = archive.by_index_raw(i).ok()?;
            if file.name() == WORKBOOK {
                continue;
            }
            writer.raw_copy_file(file).ok()?;
        }
        writer
            .start_file(WORKBOOK, zip::write::SimpleFileOptions::default())
            .ok()?;
        writer.write_all(patched.as_bytes()).ok()?;
        writer.finish().ok()?;
    }
    Some(out.into_inner())
}

fn read_entry(archive: &mut ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    use std::io::Read;
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = String::new();
    entry.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Relationship ids whose Type is a plain worksheet.
fn worksheet_rel_ids(rels_xml: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for fragment in rels_xml.split("<Relationship ") {
        let Some(ty) = attr(fragment, "Type") else {
            continue;
        };
        if !ty.ends_with(WORKSHEET_REL_SUFFIX) {
            continue;
        }
        if let Some(id) = attr(fragment, "Id") {
            ids.push(id);
        }
    }
    ids
}

/// Remove `<sheet .../>` entries that do not point at a readable worksheet.
/// Returns the patched XML plus how many sheets were dropped and kept.
fn strip_unopenable_sheets(workbook_xml: &str, worksheet_ids: &[String]) -> (String, usize, usize) {
    let mut out = String::with_capacity(workbook_xml.len());
    let mut rest = workbook_xml;
    let (mut dropped, mut kept) = (0usize, 0usize);

    while let Some(start) = rest.find("<sheet ") {
        let Some(len) = rest[start..].find('>') else {
            break;
        };
        let end = start + len + 1;
        let element = &rest[start..end];
        // Guard against matching <sheetPr>, <sheetView>, … — only the space
        // after "sheet" makes this a <sheet> element, which `find` already
        // enforces, but a self-closing check keeps it honest.
        let rid = attr(element, "r:id").or_else(|| attr(element, "id"));
        let keep = rid
            .as_deref()
            .is_some_and(|id| !id.is_empty() && worksheet_ids.iter().any(|w| w == id));

        out.push_str(&rest[..start]);
        if keep {
            out.push_str(element);
            kept += 1;
        } else {
            dropped += 1;
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    (out, dropped, kept)
}

/// Read `name="value"` from an XML fragment.
fn attr(fragment: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = fragment.find(&needle)? + needle.len();
    let rest = &fragment[start..];
    Some(rest[..rest.find('"')?].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELS: &str = r#"<Relationships>
<Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet" Target="macrosheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

    #[test]
    fn keeps_worksheets_and_drops_macrosheets() {
        let wb = r#"<workbook><sheets><sheet name="Macro1" sheetId="3" r:id="rId1"/><sheet name="Sheet1" sheetId="1" r:id="rId2"/></sheets></workbook>"#;
        let (patched, dropped, kept) = strip_unopenable_sheets(wb, &worksheet_rel_ids(RELS));
        assert_eq!((dropped, kept), (1, 1));
        assert!(!patched.contains("Macro1"));
        assert!(patched.contains("Sheet1"));
        assert!(patched.contains("</sheets></workbook>"));
    }

    #[test]
    fn drops_sheets_whose_relationship_id_is_empty() {
        let wb = r#"<workbook><sheets><sheet name="Ghost" r:id=""/><sheet name="Sheet1" r:id="rId2"/></sheets></workbook>"#;
        let (patched, dropped, kept) = strip_unopenable_sheets(wb, &worksheet_rel_ids(RELS));
        assert_eq!((dropped, kept), (1, 1));
        assert!(!patched.contains("Ghost"));
    }

    #[test]
    fn a_healthy_workbook_is_left_alone() {
        let wb = r#"<workbook><sheets><sheet name="Sheet1" r:id="rId2"/></sheets></workbook>"#;
        let (_, dropped, kept) = strip_unopenable_sheets(wb, &worksheet_rel_ids(RELS));
        assert_eq!((dropped, kept), (0, 1));
    }

    #[test]
    fn other_sheet_prefixed_elements_are_not_touched() {
        let wb = r#"<workbook><sheetPr/><sheets><sheet name="Sheet1" r:id="rId2"/></sheets><sheetView tabSelected="1"/></workbook>"#;
        let (patched, dropped, _) = strip_unopenable_sheets(wb, &worksheet_rel_ids(RELS));
        assert_eq!(dropped, 0);
        assert!(patched.contains("<sheetPr/>"));
        assert!(patched.contains("<sheetView tabSelected=\"1\"/>"));
    }
}
