//! Recipient property streams have an 8-byte header, not 24.
//!
//! [MS-OXMSG] §2.4.1: the `__properties_version1.0` header is 32 bytes at the
//! top level, 24 inside an *embedded-message* storage, and **8** inside a
//! recipient or attachment storage. The reader used 24 for all sub-storages.
//! Because 24 = 8 + 16, the scan stayed entry-aligned and simply SKIPPED THE
//! FIRST PROPERTY — which for recipients is typically `PidTagRecipientType`,
//! so `.unwrap_or(1)` defaulted every recipient to "To". Measured across the
//! corpus: 8 of 11 recipient streams keep the type at offset 8, and the empty
//! `Cc:` column in their rendered output WAS this bug.
//!
//! No corpus fixture can pin it (the only file with a real Cc keeps the
//! property at offset 72, reachable under both readings), so this builds a
//! minimal specimen with the same `cfb` crate the reader uses.

use std::io::Write;

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn craft_msg(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("crafted_cc.msg");
    let mut c = cfb::create(&path).expect("create cfb");

    // Top-level subject so the document is non-empty.
    let mut s = c.create_stream("/__substg1.0_0037001F").unwrap();
    s.write_all(&utf16le("Crafted")).unwrap();
    drop(s);

    c.create_storage("/__recip_version1.0_#00000000").unwrap();
    let mut s = c
        .create_stream("/__recip_version1.0_#00000000/__substg1.0_3001001F")
        .unwrap();
    s.write_all(&utf16le("Carol Cc")).unwrap();
    drop(s);

    // 8-byte reserved header, then one 16-byte entry:
    // [type u16][pid u16][flags u32][value u64] — PtypInteger32 0x0003,
    // PidTagRecipientType 0x0C15, value 2 (Cc).
    let mut props = vec![0u8; 8];
    props.extend_from_slice(&0x0003u16.to_le_bytes());
    props.extend_from_slice(&0x0C15u16.to_le_bytes());
    props.extend_from_slice(&0u32.to_le_bytes());
    props.extend_from_slice(&2u64.to_le_bytes());
    let mut s = c
        .create_stream("/__recip_version1.0_#00000000/__properties_version1.0")
        .unwrap();
    s.write_all(&props).unwrap();
    drop(s);
    c.flush().unwrap();
    path
}

#[test]
fn a_cc_recipient_at_the_spec_offset_is_labelled_cc() {
    let dir = std::env::temp_dir().join("rs_chunks_msg_header_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = craft_msg(&dir);

    let md = chunks_rs::formats::msg::to_markdown(path.to_str().unwrap())
        .expect("crafted msg must parse");
    assert!(
        md.contains("**Cc:** Carol Cc"),
        "the recipient type at offset 8 was not read — rendered: {md:?}"
    );
    assert!(
        !md.contains("**To:** Carol Cc"),
        "recipient silently defaulted to To: {md:?}"
    );
}

/// The corpus regression net: files whose type IS at offset 8 and genuinely
/// "To" must keep rendering To — the fix must not shift anything for them.
#[test]
fn a_real_to_recipient_still_renders_to() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/msg/poi_attachment_msg_pdf.msg");
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    let md = chunks_rs::formats::msg::to_markdown(p.to_str().unwrap()).expect("must parse");
    assert!(
        md.contains("**To:** Nick Booth"),
        "a genuine To recipient regressed: {md:?}"
    );
}
