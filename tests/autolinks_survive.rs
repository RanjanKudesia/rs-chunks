//! CommonMark autolinks must survive `strip_inline`; raw HTML must not.
//!
//! Regression test for TECH_DEBT L4. `strip_inline` treated any `<` followed by
//! a letter or `/` as the start of raw inline HTML and discarded through the
//! next `>`. That is correct for real `.md` — and wrong for every format that
//! renders *to* markdown, where `<…>` is ordinary text. The most visible damage
//! was email addresses: measured over the corpus, **34 addresses vanished from
//! `.eml` chunk content, 210 from `.mbox`, 10 from `.msg`**, so
//! `**From:** John X. Doe <bbb@ddd.com>` reached the chunk as
//! `From: John X. Doe`.
//!
//! The fix is not escaping — it is that `<user@host>` and `<scheme:…>` were
//! never raw HTML in the first place. CommonMark calls them *autolinks*, and
//! they render as their own text. So `get_markdown` output is unchanged and
//! `.md` keeps exactly its CommonMark behaviour; only the misclassification is
//! gone.

use chunks_rs::get_chunks;
use std::io::Write;

fn chunk_text(name: &str, body: &str) -> String {
    let dir = std::env::temp_dir().join("chunks_rs_autolink_test");
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("write fixture");
    f.write_all(body.as_bytes()).expect("write fixture");
    // (mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
    let chunks = get_chunks(&path.to_string_lossy(), "default", 3, 1, 3, 15).expect("chunks");
    chunks
        .iter()
        .map(|c| c.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn email_autolinks_are_kept() {
    let out = chunk_text("email.md", "Mail me at <someone@example.com> today.\n");
    assert!(
        out.contains("someone@example.com"),
        "email autolink was dropped: {out:?}"
    );
}

#[test]
fn uri_autolinks_are_kept() {
    let out = chunk_text("uri.md", "See <https://example.com/page> for details.\n");
    assert!(
        out.contains("https://example.com/page"),
        "URI autolink was dropped: {out:?}"
    );
}

#[test]
fn raw_html_is_still_stripped() {
    // The other half of the contract: this must NOT regress into keeping tags.
    let out = chunk_text(
        "tags.md",
        "A <span class=\"x\">tagged</span> word and <b>bold</b> text.\n",
    );
    assert!(!out.contains("<span"), "raw HTML leaked: {out:?}");
    assert!(!out.contains("<b>"), "raw HTML leaked: {out:?}");
    assert!(out.contains("tagged"), "tag text was lost: {out:?}");
    assert!(out.contains("bold"), "tag text was lost: {out:?}");
}

#[test]
fn a_tag_with_a_colon_in_an_attribute_is_not_mistaken_for_an_autolink() {
    // `<a href="https://x">` contains a colon, but also spaces — the whitespace
    // rule is what keeps it classified as HTML rather than a URI autolink.
    let out = chunk_text(
        "attr.md",
        "Link: <a href=\"https://example.com\">click</a>.\n",
    );
    assert!(!out.contains("<a href"), "HTML leaked: {out:?}");
    assert!(out.contains("click"), "link text was lost: {out:?}");
}

#[test]
fn a_bare_angle_pair_is_not_an_autolink() {
    // No `@` and no `:` — not an autolink, and not a tag either. Previously the
    // tag branch ate it; it should stay eaten (unchanged behaviour) rather than
    // silently start appearing.
    let out = chunk_text("bare.md", "Compare <div> and </div> markers.\n");
    assert!(!out.contains("<div>"), "unexpected change: {out:?}");
}
