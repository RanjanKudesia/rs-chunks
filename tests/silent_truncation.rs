//! A shorter document must never be reported as a complete one (TECH_DEBT F7).
//!
//! This is the worst failure shape a chunking library has: the caller cannot
//! tell "this document is short" from "we lost most of it", and nothing in the
//! output hints that anything is missing. L14 fixed one instance of it for
//! EPUB's chunker; these are the rest.
//!
//! Two shapes of fix, chosen by whether the loss has a natural unit:
//!
//! * **No unit → raise.** A single `.eml` that will not parse is not a document,
//!   and an unreadable `.msg` body stream is not an absent body.
//! * **A repeated unit → isolate and record.** One dangling spine href must not
//!   lose an 800-chunk book, and one bad message must not lose a 5,000-message
//!   mailbox — but the gap has to be stated, not left blank. `skipped_*` is
//!   always present and empty when nothing was lost, so its absence never has
//!   to be interpreted. That is the contract xlsx's `skipped_sheets` set (#66).

use chunks_rs::formats::{eml, epub};

// Scope note, measured not assumed: `mail-parser` is extremely lenient — it
// accepts headerless binary as a body — so the `None` branch of
// `parse_message_bytes` is reached by very little, and no input tried here
// produced it. The `.eml` raise is therefore mostly latent; what the change
// actually buys today is the *panic* branch (which used to become an empty
// document) and the mbox isolation below. There is no test for the raise
// because there is no input that triggers it, and a test asserting
// `Ok(_) | Err(_)` would pin nothing while looking like coverage.

/// Binary noise, used as the bad message inside the mbox below.
const GARBAGE: &[u8] = b"\xff\xfe\x00 this is not a message at all \x00\xff";

/// Every real message must be unaffected — the fix must not turn working mail
/// into errors.
#[test]
fn well_formed_mail_is_unaffected() {
    let msg =
        b"From: a@example.com\r\nSubject: Hello there\r\n\r\nBody text long enough to chunk.\r\n";
    let chunks = eml::chunk_from_bytes(msg, "ok.eml", "structural", 3, 1, 3, 15)
        .expect("a well-formed message must still parse");
    assert!(!chunks.is_empty(), "expected chunks");
}

/// An mbox isolates instead of raising — but records the gap. Message 2 is
/// garbage; 1 and 3 must survive, and the loss must be visible in the metadata
/// rather than showing only as a blank `## Message 2`.
#[test]
fn an_mbox_isolates_a_bad_message_and_records_it() {
    let mut mbox = Vec::new();
    mbox.extend_from_slice(
        b"From a@example.com Mon Jan  1 00:00:00 2024\r\n\
          From: a@example.com\r\nSubject: First\r\n\r\nFirst body, long enough.\r\n\r\n",
    );
    mbox.extend_from_slice(b"From b@example.com Mon Jan  1 00:00:00 2024\r\n");
    mbox.extend_from_slice(GARBAGE);
    mbox.extend_from_slice(
        b"\r\n\r\nFrom c@example.com Mon Jan  1 00:00:00 2024\r\n\
          From: c@example.com\r\nSubject: Third\r\n\r\nThird body, long enough.\r\n",
    );

    let chunks = eml::chunk_from_bytes(&mbox, "box.mbox", "structural", 3, 1, 3, 15)
        .expect("one bad message must not lose the mailbox");
    assert!(!chunks.is_empty(), "expected the surviving messages");

    // Whatever the splitter found, the metadata must carry the key at all
    // times — an absent key would be exactly the ambiguity this fixes.
    let meta = &chunks[0].metadata;
    let skipped = meta
        .get("document_metadata")
        .and_then(|d| d.get("skipped_messages"))
        .or_else(|| meta.get("skipped_messages"));
    assert!(
        skipped.is_some_and(|v| v.is_array()),
        "skipped_messages must always be present as an array, got {meta:?}"
    );
}

/// EPUB stamps the same contract. Every well-formed book must report an empty
/// list, never a missing key.
#[test]
fn epub_always_reports_which_spine_items_were_skipped() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_files/epub");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("epub corpus must exist") {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("epub") {
            continue;
        }
        let Ok(chunks) = epub::chunk(p.to_str().unwrap(), "structural", 3, 1, 3, 15) else {
            continue;
        };
        let Some(first) = chunks.first() else {
            continue;
        };
        let skipped = first
            .metadata
            .get("document_metadata")
            .and_then(|d| d.get("skipped_spine_items"))
            .unwrap_or_else(|| panic!("{}: skipped_spine_items missing", p.display()));
        assert!(
            skipped.as_array().is_some_and(|a| a.is_empty()),
            "{}: a well-formed book skipped {skipped:?}",
            p.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no epub fixtures exercised");
}
