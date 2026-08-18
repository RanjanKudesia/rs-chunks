//! TECH_DEBT #46: a chunk built from several JSON records must say which.
//!
//! `section`, `semantic` and `page_aware` group blocks, and for a record-based
//! format that means concatenating unrelated records with nothing to tell a
//! consumer where one ends. The chunkers are shared with every other prose
//! format, so the provenance is tracked internally as *block* indices and only
//! surfaced as `record_range` where records exist.

use chunks_rs::formats::json;

const MODES: &[&str] = &[
    "default",
    "structural",
    "section",
    "semantic",
    "sentence",
    "page_aware",
    "sliding_window",
];

fn fixture(name: &str) -> String {
    let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "test_files", name]
        .iter()
        .collect();
    path.to_string_lossy().to_string()
}

fn ranges(name: &str, mode: &str) -> Vec<(u64, u64)> {
    json::chunk(&fixture(name), mode, 3, 1, 3, 15)
        .expect("chunk")
        .into_iter()
        .map(|c| {
            let range = c
                .metadata
                .get("record_range")
                .expect("every json chunk names its records");
            let array = range.as_array().expect("record_range is a pair");
            (array[0].as_u64().unwrap(), array[1].as_u64().unwrap())
        })
        .collect()
}

/// The fixture the tracker measured: 25 products, one per line.
#[test]
fn every_mode_names_the_records_a_chunk_came_from() {
    for mode in MODES {
        let ranges = ranges("jsonl/elastic_products.ndjson", mode);
        assert!(!ranges.is_empty(), "{mode}: no chunks");
        for (first, last) in &ranges {
            assert!(first <= last, "{mode}: reversed range {first}..{last}");
            assert!(
                *last < 25,
                "{mode}: record {last} is past the end of a 25-record file"
            );
        }
        assert_eq!(
            ranges.first().unwrap().0,
            0,
            "{mode}: the first chunk skips record 0"
        );
        assert_eq!(
            ranges.last().unwrap().1,
            24,
            "{mode}: the last chunk stops short"
        );
    }
}

/// One chunk per record in the element-level modes, so the range is a point.
#[test]
fn element_level_modes_map_one_record_to_one_chunk() {
    for mode in ["default", "structural"] {
        let ranges = ranges("jsonl/elastic_products.ndjson", mode);
        assert_eq!(ranges.len(), 25, "{mode}");
        for (i, (first, last)) in ranges.iter().enumerate() {
            assert_eq!((*first, *last), (i as u64, i as u64), "{mode}: chunk {i}");
        }
    }
}

/// The grouping modes are the point of #46: their chunks must partition the
/// records, with no gap and no chunk claiming a record it does not contain.
#[test]
fn grouping_modes_partition_the_records_without_gaps() {
    for mode in ["section", "semantic", "page_aware"] {
        let ranges = ranges("jsonl/elastic_products.ndjson", mode);
        assert!(
            ranges.len() > 1,
            "{mode}: expected several groups, got {}",
            ranges.len()
        );
        for pair in ranges.windows(2) {
            assert_eq!(
                pair[1].0,
                pair[0].1 + 1,
                "{mode}: {:?} then {:?} leaves a gap or overlaps",
                pair[0],
                pair[1]
            );
        }
    }
}

/// Overlapping windows *should* overlap — that is what the mode is.
#[test]
fn sliding_window_ranges_overlap_by_the_overlap() {
    let ranges = ranges("jsonl/elastic_products.ndjson", "sliding_window");
    for pair in ranges.windows(2) {
        assert!(
            pair[1].0 <= pair[0].1,
            "windows {:?} and {:?} do not overlap",
            pair[0],
            pair[1]
        );
    }
}

/// A `.json` document, where records come from an array rather than lines.
#[test]
fn a_json_array_document_is_mapped_too() {
    let ranges = ranges("json/geojson_countries.json", "section");
    assert!(!ranges.is_empty());
    assert_eq!(ranges.first().unwrap().0, 0);
    for (first, last) in &ranges {
        assert!(first <= last);
    }
}

/// `record_count` at chunk level would collide with `document_metadata`'s, which
/// counts the whole file. The range carries the same information unambiguously.
#[test]
fn a_chunk_does_not_shadow_the_documents_record_count() {
    for mode in MODES {
        for chunk in
            json::chunk(&fixture("jsonl/elastic_products.ndjson"), mode, 3, 1, 3, 15).unwrap()
        {
            assert!(
                chunk.metadata.get("record_count").is_none(),
                "{mode}: a chunk-level record_count would shadow the document's"
            );
            assert_eq!(
                chunk.metadata["document_metadata"]["record_count"]
                    .as_u64()
                    .unwrap(),
                25,
                "{mode}: the document's own record count changed meaning"
            );
        }
    }
}

/// A format with no records must gain nothing. The provenance rides on an
/// internal field precisely so that adding it changes no one else's metadata.
#[test]
fn formats_without_records_gain_no_key() {
    let mut checked = 0;
    for (name, chunker) in [
        ("md/prose_heavy.md", "md"),
        ("rtf/conv_libreoffice_heading123.rtf", "rtf"),
    ] {
        let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "test_files", name]
            .iter()
            .collect();
        assert!(
            path.exists(),
            "{name} is missing — the check would silently pass"
        );
        let path = path.to_string_lossy().to_string();
        let chunks = match chunker {
            "md" => chunks_rs::formats::md::chunk(&path, "section", 3, 1, 3, 15),
            _ => chunks_rs::formats::rtf::chunk(&path, "section", 3, 1, 3, 15),
        };
        for chunk in chunks.expect("chunk") {
            assert!(
                chunk.metadata.get("record_range").is_none(),
                "{name} gained a record_range it has no records for"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no chunks were examined");
}
