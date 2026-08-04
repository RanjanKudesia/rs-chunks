//! SmartArt (DrawingML diagram) text extraction.
//!
//! A SmartArt graphic contributes no text to the slide XML at all. The slide
//! only carries a `<p:graphicFrame>` holding `<dgm:relIds r:dm="rIdN"/>`, which
//! points through the slide's `.rels` at `ppt/diagrams/dataN.xml`. That part —
//! the diagram *data model* — is where the user's words live. Read the slide
//! alone and every SmartArt label is silently lost.
//!
//! The rendered `drawing1.xml` is deliberately ignored: it is a cached picture
//! of the diagram that PowerPoint may omit or leave stale, and it duplicates
//! text the data model already holds authoritatively.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufReader;

use super::common::{local_name, read_zip_entry, resolve_relative_path, PptxArchive};

/// Relationship type of the diagram data part, as written in a slide's `.rels`.
const DIAGRAM_DATA_REL: &str = "diagramData";

/// Resolve a slide's `<dgm:relIds r:dm=…>` values to `ppt/diagrams/dataN.xml`
/// paths via that slide's `.rels`. Unknown ids are skipped rather than erroring
/// — a diagram whose relationship is missing is a damaged file, not a reason to
/// fail the whole deck.
pub fn resolve_diagram_parts(
    archive: &mut PptxArchive,
    slide_name: &str,
    rids: &[String],
) -> Vec<String> {
    if rids.is_empty() {
        return Vec::new();
    }
    let Some(last_slash) = slide_name.rfind('/') else {
        return Vec::new();
    };
    let (dir, file) = (&slide_name[..last_slash], &slide_name[last_slash + 1..]);
    let Ok(bytes) = read_zip_entry(archive, &format!("{dir}/_rels/{file}.rels")) else {
        return Vec::new();
    };
    let Ok(content) = std::str::from_utf8(&bytes) else {
        return Vec::new();
    };

    let mut parts = Vec::new();
    for rid in rids {
        for rel in content.split("<Relationship ") {
            if !rel.contains(DIAGRAM_DATA_REL) {
                continue;
            }
            if attr(rel, "Id").as_deref() != Some(rid.as_str()) {
                continue;
            }
            if let Some(target) = attr(rel, "Target") {
                let path = resolve_relative_path(dir, &target);
                if !parts.contains(&path) {
                    parts.push(path);
                }
            }
            break;
        }
    }
    parts
}

/// Read `name="value"` out of one `<Relationship …>` fragment.
fn attr(fragment: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = fragment.find(&needle)? + needle.len();
    let rest = &fragment[start..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Extract the paragraphs of a diagram data part, in document order.
///
/// Text lives at `<dgm:ptLst>/<dgm:pt>/<dgm:t>/<a:p>/<a:r>/<a:t>`. Points marked
/// `type="pres"` are layout scaffolding the renderer generates — they mirror
/// content points, so including them would duplicate every label.
pub fn parse_diagram_xml(xml_bytes: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(BufReader::new(xml_bytes));
    let mut buf = Vec::new();
    let mut out: Vec<String> = Vec::new();

    let mut in_pt = false; // inside a content <dgm:pt>
    let mut pt_depth = 0i32; // nesting guard: <dgm:pt> can contain <dgm:pt>
    let mut in_dgm_t = false;
    let mut in_para = false;
    let mut in_t = false;
    let mut para_text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) => match local_name(e.name()).as_slice() {
                b"pt" => {
                    pt_depth += 1;
                    if pt_depth == 1 {
                        in_pt = !is_presentation_point(e.attributes());
                    }
                }
                b"t" if in_pt && pt_depth > 0 && !in_dgm_t => in_dgm_t = true,
                b"p" if in_dgm_t => {
                    in_para = true;
                    para_text.clear();
                }
                b"t" if in_para => in_t = true,
                _ => {}
            },
            Ok(Event::Text(ref e)) => {
                if in_t {
                    para_text.push_str(e.decode().unwrap_or_default().as_ref());
                }
            }
            Ok(Event::End(ref e)) => match local_name(e.name()).as_slice() {
                b"t" if in_t => in_t = false,
                b"t" if in_dgm_t => in_dgm_t = false,
                b"p" if in_para => {
                    in_para = false;
                    let trimmed = para_text.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_string());
                    }
                    para_text.clear();
                }
                b"pt" if pt_depth > 0 => {
                    pt_depth -= 1;
                    if pt_depth == 0 {
                        in_pt = false;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}

/// `<dgm:pt type="pres">` — a presentation node the layout engine generated.
fn is_presentation_point(attrs: quick_xml::events::attributes::Attributes<'_>) -> bool {
    for attr in attrs.flatten() {
        let key = attr.key.as_ref();
        let local = key.rsplit(|b| *b == b':').next().unwrap_or(key);
        if local == b"type" {
            return attr.unescape_value().map(|v| v == "pres").unwrap_or(false);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = br#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a">
<dgm:ptLst>
<dgm:pt modelId="{P0}" type="doc"><dgm:t><a:p><a:endParaRPr/></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="{P1}"><dgm:t><a:p><a:r><a:t>Alpha step</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="{P2}"><dgm:t><a:p><a:r><a:t>Beta </a:t></a:r><a:r><a:t>step</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="{P3}" type="pres"><dgm:t><a:p><a:r><a:t>Alpha step</a:t></a:r></a:p></dgm:t></dgm:pt>
</dgm:ptLst></dgm:dataModel>"#;

    #[test]
    fn extracts_point_text_in_order() {
        assert_eq!(parse_diagram_xml(SAMPLE), vec!["Alpha step", "Beta step"]);
    }

    #[test]
    fn skips_presentation_points_so_labels_are_not_duplicated() {
        assert_eq!(parse_diagram_xml(SAMPLE).iter().filter(|p| *p == "Alpha step").count(), 1);
    }

    #[test]
    fn empty_diagram_yields_nothing() {
        let xml = br#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>
<dgm:pt modelId="{P0}"><dgm:t><a:p><a:endParaRPr lang="ru-RU"/></a:p></dgm:t></dgm:pt>
</dgm:ptLst></dgm:dataModel>"#;
        assert!(parse_diagram_xml(xml).is_empty());
    }

    #[test]
    fn malformed_xml_yields_nothing_rather_than_panicking() {
        assert!(parse_diagram_xml(b"<dgm:ptLst><dgm:pt><dgm:t><a:p><a:r><a:t>x").is_empty());
    }
}
