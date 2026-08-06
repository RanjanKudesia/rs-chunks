//! The hand-rolled LZFu decompressor, exercised against real Outlook messages.
//!
//! TECH_DEBT #49 recorded this path as untested and asked for "an RTF-only
//! fixture". That remedy was wrong, and the mistake is worth naming: **13 of the
//! 17 `.msg` fixtures already carry a valid `PidTagRtfCompressed` stream**, so
//! the decompressor could be tested at any time. What no fixture reaches is
//! something narrower — `read_body`'s *fallthrough* to the RTF branch, which
//! only happens when a message has compressed RTF and **no** plain or HTML body.
//! Every one of the 13 also has a plain body, so `read_body` returns before it
//! gets there.
//!
//! So the untested surface splits in two, and only the second half needs a new
//! fixture. This file closes the first half by calling the decompressor
//! directly on bytes that are already in the corpus.
//!
//! Note: `__nameid_version1.0` contains streams *named* `__substg1.0_10090102`
//! which are named-property hash buckets, not this property — their leading
//! bytes are not `LZFu`. These tests read the root storage only, which is also
//! all the engine ever looks at.

use chunks_rs::formats::msg::rtf::{compressed_rtf_to_text, decompress_rtf};

/// Fixtures with a root `PidTagRtfCompressed`, chosen to span the shapes:
/// the smallest of each RTF flavour, a non-Latin codepage, and the largest —
/// which cycles the 4,096-byte dictionary many times over.
const CASES: &[(&str, usize)] = &[
    ("tika_testMSG.msg", 910),                 // smallest \fromtext
    ("tika_testMSG_StickyNote.msg", 247),      // smallest native RTF
    ("tika_testMSG_chinese.msg", 8_711),       // \ansicpg950
    ("poi_51873.msg", 377),
    ("tika_testMSG_Contact.msg", 9_785),
    ("tika_test-outlook.msg", 26_028),         // largest — dictionary wraparound
];

fn compressed_stream(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/msg")
        .join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut cfb = cfb::CompoundFile::open(std::io::Cursor::new(bytes))
        .unwrap_or_else(|e| panic!("{name}: not a CFB: {e}"));
    let mut stream = cfb
        .open_stream("/__substg1.0_10090102")
        .unwrap_or_else(|e| panic!("{name}: no PidTagRtfCompressed: {e}"));
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut out).unwrap();
    out
}

/// The header's `rawSize` is the decompressor's own statement of what it should
/// produce, so it is a self-checking oracle on real data.
#[test]
fn decompresses_every_fixture_to_its_declared_size() {
    for (name, expected) in CASES {
        let compressed = compressed_stream(name);
        assert!(
            compressed.len() > 16,
            "{name}: stream too short to be LZFu"
        );
        assert_eq!(
            &compressed[8..12],
            b"LZFu",
            "{name}: not a compressed stream (uncompressed RTF has magic MELA)"
        );

        let raw_size = u32::from_le_bytes(compressed[4..8].try_into().unwrap()) as usize;
        assert_eq!(raw_size, *expected, "{name}: fixture changed shape");

        let out = decompress_rtf(&compressed)
            .unwrap_or_else(|e| panic!("{name}: decompression failed: {e}"));
        assert_eq!(
            out.len(),
            raw_size,
            "{name}: produced {} bytes, header declares {raw_size}",
            out.len()
        );
        assert!(
            out.starts_with(b"{\\rtf"),
            "{name}: output is not RTF: {:?}",
            String::from_utf8_lossy(&out[..out.len().min(24)])
        );
    }
}

/// The dictionary is pre-loaded with 207 bytes of common RTF control words and
/// wraps at 4,096. `tika_test-outlook.msg` expands 6.6 KB to 26 KB, so it wraps
/// repeatedly — a fixture short enough never to wrap would not test the ring.
#[test]
fn the_largest_fixture_cycles_the_dictionary() {
    let compressed = compressed_stream("tika_test-outlook.msg");
    let out = decompress_rtf(&compressed).expect("decompression failed");
    assert!(
        out.len() > 4096 * 6,
        "expected the dictionary to wrap several times, got {} bytes",
        out.len()
    );
    assert!(String::from_utf8_lossy(&out).contains("\\rtf1"));
}

/// The full pipeline: decompress, parse the RTF, normalise whitespace. This is
/// what `read_body` would call, and it is the half of the path that turns bytes
/// into something a chunk can hold.
#[test]
fn compressed_rtf_becomes_readable_text() {
    let compressed = compressed_stream("tika_testMSG.msg");
    let text = compressed_rtf_to_text(&compressed).expect("no text recovered");
    assert!(!text.trim().is_empty(), "recovered no text at all");
    assert!(
        !text.contains("\\rtf") && !text.contains("\\par"),
        "RTF control words leaked into the text: {:?}",
        &text[..text.len().min(120)]
    );
}

/// Non-Latin content must survive the codepage, not become replacement
/// characters — the `.msg` reader has had exactly that defect before (#47).
#[test]
fn a_non_latin_codepage_survives() {
    let compressed = compressed_stream("tika_testMSG_chinese.msg");
    let text = compressed_rtf_to_text(&compressed).expect("no text recovered");
    assert!(
        !text.contains('\u{FFFD}'),
        "replacement characters in the decoded body"
    );
    assert!(
        text.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
        "expected CJK characters in a \\ansicpg950 message"
    );
}

/// Adversarial input must fail cleanly rather than panic or allocate wildly —
/// `rawSize` is attacker-controlled in a hostile file.
#[test]
fn malformed_streams_fail_cleanly() {
    assert!(decompress_rtf(&[]).is_err(), "empty input");
    assert!(decompress_rtf(b"short").is_err(), "truncated header");

    // Valid header, absurd rawSize, no data.
    let mut hostile = Vec::new();
    hostile.extend_from_slice(&16u32.to_le_bytes());
    hostile.extend_from_slice(&u32::MAX.to_le_bytes());
    hostile.extend_from_slice(b"LZFu");
    hostile.extend_from_slice(&0u32.to_le_bytes());
    let result = decompress_rtf(&hostile);
    assert!(
        result.is_err() || result.as_ref().is_ok_and(|v| v.len() < 1_000_000),
        "a bogus rawSize must not be trusted"
    );
}
