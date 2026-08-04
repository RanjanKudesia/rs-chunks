//! Embedded chart data (DrawingML charts).
//!
//! Sibling of [`super::diagram`], and the same shape: a slide carries only a
//! `<p:graphicFrame>` holding `<c:chart r:id="rIdN"/>`, which resolves through
//! the slide's `.rels` to `ppt/charts/chartN.xml`. That part holds the plotted
//! numbers, and none of it was ever read.
//!
//! Two decisions, both forced by what our fixtures actually contain:
//!
//! * **No title.** `<c:title>` is empty (`<c:layout/><c:overlay val="0"/>`) in
//!   every chart we have, and one is absent entirely — there is not a single
//!   `<a:t>` in any chart part. Leading with the title would emit "Chart: "
//!   every time. The human-readable title lives on the *slide*, and already
//!   extracts.
//! * **No embedded workbook.** `<c:externalData>` points at
//!   `ppt/embeddings/*.xlsx`, but the `<c:strCache>`/`<c:numCache>` inside the
//!   chart is complete and authoritative — checked against the workbook, whose
//!   corresponding cells are genuinely empty. Recursing into a nested zip would
//!   cost a lot for nothing.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufReader;

use super::common::{local_name, read_zip_entry, resolve_relative_path, PptxArchive};

/// Relationship type of a chart part, as written in a slide's `.rels`.
///
/// Matched on the suffix, not with `contains("chart")` — a chart's own rels
/// also reference `chartUserShapes`, which holds stale rendered data labels.
const CHART_REL_SUFFIX: &str = "/chart";

/// Widest table we will build from one chart. Our own fixtures top out at
/// 2 series x 11 categories; the cap only guards against a foreign deck.
const MAX_SERIES: usize = 24;
const MAX_CATEGORIES: usize = 200;

/// Resolve `<c:chart r:id=…>` values to `ppt/charts/chartN.xml` paths.
pub fn resolve_chart_parts(
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
            let Some(ty) = attr(rel, "Type") else { continue };
            if !ty.ends_with(CHART_REL_SUFFIX) {
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

fn attr(fragment: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = fragment.find(&needle)? + needle.len();
    let rest = &fragment[start..];
    Some(rest[..rest.find('"')?].to_string())
}

/// One plotted series: its name and its values keyed by category index.
#[derive(Default)]
struct Series {
    name: Option<String>,
    /// `(category index, value)`. Keyed by `<c:pt idx>`, never by position —
    /// caches are sparse. In sample.pptx one series has `ptCount val="11"` but
    /// supplies only indices 7..10, so reading them in order would shift every
    /// value seven columns to the left.
    points: Vec<(usize, String)>,
}

/// Which cache we are currently reading inside a `<c:ser>`.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    None,
    Name,
    Category,
    Value,
}

/// Extract a chart as rows: a header (`Category`, then each series name)
/// followed by one row per category. Empty when the chart holds no cached data.
pub fn parse_chart_xml(xml_bytes: &[u8]) -> Vec<Vec<String>> {
    let mut reader = Reader::from_reader(BufReader::new(xml_bytes));
    let mut buf = Vec::new();

    let mut series: Vec<Series> = Vec::new();
    let mut categories: Vec<(usize, String)> = Vec::new();
    let mut slot = Slot::None;
    let mut in_ser = false;
    let mut pt_idx: usize = 0;
    let mut in_v = false;
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                match local_name(e.name()).as_slice() {
                    b"ser" => {
                        in_ser = true;
                        if series.len() < MAX_SERIES {
                            series.push(Series::default());
                        }
                    }
                    b"tx" if in_ser => slot = Slot::Name,
                    b"cat" if in_ser => slot = Slot::Category,
                    b"val" if in_ser => slot = Slot::Value,
                    b"pt" => {
                        pt_idx = attr_usize(e, b"idx").unwrap_or(0);
                    }
                    b"v" => {
                        in_v = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_v {
                    text.push_str(e.decode().unwrap_or_default().as_ref());
                }
            }
            Ok(Event::End(ref e)) => match local_name(e.name()).as_slice() {
                b"v" if in_v => {
                    in_v = false;
                    let value = text.trim().to_string();
                    if !value.is_empty() {
                        match slot {
                            Slot::Name => {
                                if let Some(s) = series.last_mut() {
                                    s.name.get_or_insert(value);
                                }
                            }
                            Slot::Category => {
                                if categories.len() < MAX_CATEGORIES
                                    && !categories.iter().any(|(i, _)| *i == pt_idx)
                                {
                                    categories.push((pt_idx, value));
                                }
                            }
                            Slot::Value => {
                                if let Some(s) = series.last_mut() {
                                    s.points.push((pt_idx, trim_float(&value)));
                                }
                            }
                            Slot::None => {}
                        }
                    }
                    text.clear();
                }
                b"tx" | b"cat" | b"val" => slot = Slot::None,
                b"ser" => in_ser = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }

    build_rows(categories, series)
}

fn build_rows(mut categories: Vec<(usize, String)>, series: Vec<Series>) -> Vec<Vec<String>> {
    let has_values = series.iter().any(|s| !s.points.is_empty());
    if !has_values {
        return Vec::new();
    }
    categories.sort_by_key(|(i, _)| *i);
    // A chart can plot values with no category axis; fall back to 1-based
    // ordinals so the rows still line up.
    if categories.is_empty() {
        let max = series
            .iter()
            .flat_map(|s| s.points.iter().map(|(i, _)| *i))
            .max()
            .unwrap_or(0);
        categories = (0..=max).map(|i| (i, (i + 1).to_string())).collect();
    }

    let mut header = vec!["Category".to_string()];
    for (n, s) in series.iter().enumerate() {
        header.push(match &s.name {
            // Duplicate series names are real — sample.pptx has two both called
            // "Graph information" — so disambiguate rather than emit two
            // identical headers.
            Some(name) if series.iter().filter(|o| o.name.as_ref() == Some(name)).count() > 1 => {
                format!("{name} ({})", n + 1)
            }
            Some(name) => name.clone(),
            None => format!("Series {}", n + 1),
        });
    }

    let mut rows = vec![header];
    for (idx, label) in &categories {
        let mut row = vec![label.clone()];
        for s in &series {
            row.push(
                s.points
                    .iter()
                    .find(|(i, _)| i == idx)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
            );
        }
        rows.push(row);
    }
    rows
}

/// `8.200000000000001` is what the file says; `8.2` is what it means.
fn trim_float(value: &str) -> String {
    match value.parse::<f64>() {
        Ok(n) if n.is_finite() => {
            let s = format!("{n:.6}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            if s.is_empty() || s == "-" {
                "0".to_string()
            } else {
                s.to_string()
            }
        }
        _ => value.to_string(),
    }
}

fn attr_usize(e: &quick_xml::events::BytesStart<'_>, want: &[u8]) -> Option<usize> {
    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref();
        let local = key.rsplit(|b| *b == b':').next().unwrap_or(key);
        if local == want {
            return attr.unescape_value().ok()?.trim().parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly like poi_bar-chart.pptx's chart1.xml.
    const DENSE: &[u8] = br#"<c:chartSpace xmlns:c="c"><c:chart><c:title><c:layout/></c:title>
<c:plotArea><c:barChart><c:ser><c:idx val="0"/>
<c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Sales</c:v></c:pt></c:strCache></c:strRef></c:tx>
<c:cat><c:strRef><c:strCache><c:ptCount val="2"/>
<c:pt idx="0"><c:v>1st Qtr</c:v></c:pt><c:pt idx="1"><c:v>2nd Qtr</c:v></c:pt></c:strCache></c:strRef></c:cat>
<c:val><c:numRef><c:numCache><c:ptCount val="2"/>
<c:pt idx="0"><c:v>8.200000000000001</c:v></c:pt><c:pt idx="1"><c:v>3.2</c:v></c:pt></c:numCache></c:numRef></c:val>
</c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;

    #[test]
    fn extracts_categories_and_values() {
        let rows = parse_chart_xml(DENSE);
        assert_eq!(rows[0], vec!["Category", "Sales"]);
        assert_eq!(rows[1], vec!["1st Qtr", "8.2"]);
        assert_eq!(rows[2], vec!["2nd Qtr", "3.2"]);
    }

    #[test]
    fn float_noise_is_trimmed() {
        assert_eq!(trim_float("8.200000000000001"), "8.2");
        assert_eq!(trim_float("-1"), "-1");
        assert_eq!(trim_float("6.478999999999999"), "6.479");
        assert_eq!(trim_float("not a number"), "not a number");
    }

    /// sample.pptx has a series supplying only indices 7..10 of 11.
    #[test]
    fn sparse_points_land_on_their_own_category() {
        let xml = br#"<c:chartSpace xmlns:c="c"><c:ser>
<c:cat><c:strRef><c:strCache><c:ptCount val="3"/>
<c:pt idx="0"><c:v>2002</c:v></c:pt><c:pt idx="1"><c:v>2003</c:v></c:pt><c:pt idx="2"><c:v>2004</c:v></c:pt>
</c:strCache></c:strRef></c:cat>
<c:val><c:numRef><c:numCache><c:ptCount val="3"/>
<c:pt idx="2"><c:v>2.22</c:v></c:pt></c:numCache></c:numRef></c:val>
</c:ser></c:chartSpace>"#;
        let rows = parse_chart_xml(xml);
        assert_eq!(rows[1], vec!["2002", ""]);
        assert_eq!(rows[2], vec!["2003", ""]);
        assert_eq!(rows[3], vec!["2004", "2.22"]);
    }

    #[test]
    fn duplicate_series_names_are_disambiguated() {
        let xml = br#"<c:chartSpace xmlns:c="c">
<c:ser><c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Graph information</c:v></c:pt></c:strCache></c:strRef></c:tx>
<c:cat><c:strCache><c:pt idx="0"><c:v>A</c:v></c:pt></c:strCache></c:cat>
<c:val><c:numCache><c:pt idx="0"><c:v>1</c:v></c:pt></c:numCache></c:val></c:ser>
<c:ser><c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Graph information</c:v></c:pt></c:strCache></c:strRef></c:tx>
<c:cat><c:strCache><c:pt idx="0"><c:v>A</c:v></c:pt></c:strCache></c:cat>
<c:val><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:val></c:ser>
</c:chartSpace>"#;
        let rows = parse_chart_xml(xml);
        assert_eq!(
            rows[0],
            vec!["Category", "Graph information (1)", "Graph information (2)"]
        );
        assert_eq!(rows[1], vec!["A", "1", "2"]);
    }

    #[test]
    fn a_chart_with_no_cached_values_yields_nothing() {
        let xml = br#"<c:chartSpace xmlns:c="c"><c:chart><c:title><c:layout/></c:title></c:chart></c:chartSpace>"#;
        assert!(parse_chart_xml(xml).is_empty());
    }

    #[test]
    fn malformed_xml_yields_nothing_rather_than_panicking() {
        assert!(parse_chart_xml(b"<c:ser><c:val><c:numCache><c:pt idx=\"0\"><c:v>1").is_empty());
    }
}
