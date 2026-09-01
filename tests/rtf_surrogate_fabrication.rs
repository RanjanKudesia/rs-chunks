//! A stale half-surrogate must never synthesise a character.
//!
//! `\uN` escapes in the astral range arrive as surrogate halves. The extractor
//! held a high surrogate in `ctx.pending_surrogate` until a low one appeared —
//! with no bound on how far away that was. In
//! `test_files/rtf/tika_testRTFInvalidUnicode.rtf`, which Tika built precisely
//! to carry UNPAIRED escapes, a high surrogate from the "Unpaired hi" line
//! survived across a paragraph and paired with the low surrogate on the
//! "Unpaired lo" line, emitting U+10000 (𐀀) — a character present nowhere in
//! the source bytes.
//!
//! Losing text is a gap. Inventing it is worse: no downstream consumer can
//! detect a character that was never in the document.

use chunks_rs::formats::rtf;

fn fixture(name: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_files/rtf")
        .join(name);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p
}

#[test]
fn unpaired_surrogates_do_not_synthesise_a_character() {
    let md = rtf::to_markdown(fixture("tika_testRTFInvalidUnicode.rtf").to_str().unwrap())
        .expect("fixture must parse");
    assert!(
        !md.contains('\u{10000}'),
        "fabricated U+10000 from two unrelated escapes: {md:?}"
    );
    // The surrounding prose must survive — the fix drops half-pairs, not text.
    assert!(
        md.contains("Unpaired") && md.contains("here"),
        "the fix ate real text: {md:?}"
    );
}

/// Control: a genuine, adjacent surrogate pair must still combine. Gothic
/// U+10330 is written as the pair D800 DF30.
#[test]
fn an_adjacent_surrogate_pair_still_combines() {
    let md = rtf::to_markdown(fixture("tika_testRTFUnicodeGothic.rtf").to_str().unwrap())
        .expect("fixture must parse");
    let astral = md.chars().filter(|c| (*c as u32) >= 0x10000).count();
    assert!(
        astral > 0,
        "a real astral pair stopped combining — the fix is too aggressive: {md:?}"
    );
}
