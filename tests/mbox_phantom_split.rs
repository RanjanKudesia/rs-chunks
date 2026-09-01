//! A body line beginning `From ` is not a message.
//!
//! mboxo leaves body `From ` lines unescaped, and the splitter treated every
//! one as a postmark: `mimekit_unmunged.mbox` holds 2 messages, the engine
//! emitted `message_index` [1,2,3,4], and each phantom's "headers" were prose
//! — while the mis-detected line was deleted from the real body. A genuine
//! postmark is always followed by a header line (`Name: …`); prose is not.

use chunks_rs::formats::eml;

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/mbox")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p.to_str().unwrap().to_string()
}

fn message_count(name: &str) -> usize {
    let chunks = eml::chunk(&fixture(name), "structural", 3, 1, 5, 3).expect("must parse");
    chunks
        .iter()
        .filter_map(|c| c.metadata["message_index"].as_u64())
        .max()
        .unwrap_or(0) as usize
}

#[test]
fn an_unmunged_body_from_line_is_not_a_message() {
    // 2 real postmarks (`From -`), 2 body lines `From Russia with love`.
    assert_eq!(message_count("mimekit_unmunged.mbox"), 2, "phantom split");
}

#[test]
fn the_body_from_line_survives_in_the_text() {
    // The mis-detected line used to be DELETED as a separator.
    let md = eml::to_markdown(&fixture("mimekit_unmunged.mbox")).expect("must parse");
    assert!(
        md.contains("From Russia with love"),
        "the body line was eaten: {md:?}"
    );
}

#[test]
fn real_mailboxes_keep_their_counts() {
    // The splitter was already right on these; the discriminator must not
    // merge genuine messages. Counts are byte-audited (review, mbox.md).
    assert_eq!(message_count("mimekit_simple.mbox"), 3);
    assert_eq!(message_count("tika_complex.mbox"), 3);
    assert_eq!(message_count("tika_quoted.mbox"), 1);
}
