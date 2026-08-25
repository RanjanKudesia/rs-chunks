//! Text outside a recognised block tag must still be extracted.
//!
//! `tag_to_block_type` is a whitelist (p, h1-h6, ul/ol, table, pre, blockquote,
//! …). Everything else used to be dropped: `<div>Hello</div>` produced **zero**
//! chunks, and `<div>a</div><p>b</p>` produced one chunk containing only "b" —
//! a silent partial extraction that still reported success. Modern pages are
//! predominantly div/section/span, so this was a large class of documents
//! returning nothing or a fragment.
//!
//! These tests pin both halves: the text is now gathered, and the structure
//! that already worked is unchanged.

use std::io::Write;

fn html_file(name: &str, body: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    std::fs::File::create(&p)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    p
}

fn chunks_of(name: &str, body: &str) -> Vec<chunks_rs::Chunk> {
    let p = html_file(name, body);
    let out = chunks_rs::get_chunks(p.to_str().unwrap(), "default", 3, 1, 3, 15).unwrap();
    let _ = std::fs::remove_file(&p);
    out
}

fn text_of(name: &str, body: &str) -> String {
    chunks_of(name, body)
        .iter()
        .map(|c| c.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn text_in_a_bare_container_is_extracted() {
    for (i, tag) in [
        "div", "section", "article", "main", "span", "header", "footer",
    ]
    .iter()
    .enumerate()
    {
        let body = format!("<html><body><{tag}>The quick brown fox</{tag}></body></html>");
        let got = text_of(&format!("loose_{i}.html"), &body);
        assert!(
            got.contains("The quick brown fox"),
            "<{tag}> lost its text: {got:?}"
        );
    }
}

#[test]
fn nested_containers_do_not_hide_text() {
    let got = text_of(
        "loose_nested.html",
        r#"<html><body><div class="a"><div class="b"><span>Real content here</span></div></div></body></html>"#,
    );
    assert!(got.contains("Real content here"), "got {got:?}");
}

/// The regression the obvious fix would have caused. Making `div` a Paragraph
/// block would swallow the whole subtree, losing the heading inside it.
#[test]
fn a_heading_inside_a_container_stays_a_heading() {
    let cs = chunks_of(
        "loose_heading.html",
        "<html><body><div><h1>Title</h1><p>Body</p></div></body></html>",
    );
    assert!(
        cs.iter()
            .any(|c| c.content_type == "heading" && c.content.contains("Title")),
        "heading was absorbed: {:?}",
        cs.iter()
            .map(|c| (&c.content_type, &c.content))
            .collect::<Vec<_>>()
    );
}

#[test]
fn loose_text_and_real_blocks_keep_document_order() {
    let got = text_of(
        "loose_order.html",
        "<html><body><div>Alpha text</div><p>Beta text</p><div>Gamma text</div></body></html>",
    );
    let a = got.find("Alpha").expect("alpha missing");
    let b = got.find("Beta").expect("beta missing");
    let g = got.find("Gamma").expect("gamma missing");
    assert!(a < b && b < g, "out of order: {got:?}");
}

#[test]
fn adjacent_containers_do_not_weld_words_together() {
    let got = text_of(
        "loose_weld.html",
        "<html><body><div>Alpha</div><div>Beta</div></body></html>",
    );
    assert!(!got.contains("AlphaBeta"), "words welded: {got:?}");
    assert!(got.contains("Alpha") && got.contains("Beta"), "got {got:?}");
}

/// A browser renders `<b>bold</b>face` as one word, so gathering must not
/// insert a separator for inline elements.
#[test]
fn inline_elements_do_not_gain_a_space() {
    let got = text_of(
        "loose_inline.html",
        "<html><body><div><b>bold</b>face</div></body></html>",
    );
    assert!(got.contains("boldface"), "inline split apart: {got:?}");
}

#[test]
fn script_and_style_bodies_never_become_text() {
    let got = text_of(
        "loose_script.html",
        "<html><body><div>Keep this</div>\
         <script>var secret = 1; if (a<b) {}</script>\
         <style>.a{color:red}</style></body></html>",
    );
    assert!(got.contains("Keep this"), "got {got:?}");
    assert!(!got.contains("secret"), "script body leaked: {got:?}");
    assert!(!got.contains("color:red"), "style body leaked: {got:?}");
}

/// `<Style>` with a capital S, guarded by the `/*<![CDATA[*/` wrapper real
/// stylesheets use. `find_matching_tag_end` used to abandon its search on the
/// first `<` it could not parse as a tag, so the element was never skipped and
/// its CSS was read as prose. `tika_testEPUB.epub` ships exactly this.
#[test]
fn a_style_element_containing_angle_brackets_is_still_skipped() {
    let got = text_of(
        "loose_cdata.html",
        "<html><head><Style>/*<![CDATA[*/ p {text-align: right} /*]]>*/ nothing to see here</Style>\
         <script>nor here</script></head>\
         <body><div>Real body text</div></body></html>",
    );
    assert!(got.contains("Real body text"), "got {got:?}");
    assert!(
        !got.contains("nothing to see here"),
        "style body leaked: {got:?}"
    );
    assert!(!got.contains("nor here"), "script body leaked: {got:?}");
    assert!(!got.contains("text-align"), "css leaked: {got:?}");
}

/// Declarations are not elements. Once loose text was gathered, their bodies
/// would otherwise be read as prose — `?xml version="1.0" ...` really did
/// surface as a chunk.
#[test]
fn declarations_and_comments_are_not_emitted_as_text() {
    let got = text_of(
        "loose_decl.html",
        "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\
         <!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd\">\
         <html><body><!-- a comment with <div>markup</div> inside --><div>Only this</div></body></html>",
    );
    assert!(got.contains("Only this"), "got {got:?}");
    assert!(
        !got.contains("xml version"),
        "xml declaration leaked: {got:?}"
    );
    assert!(!got.contains("DOCTYPE"), "doctype leaked: {got:?}");
    assert!(!got.contains("a comment"), "comment leaked: {got:?}");
}

#[test]
fn lists_and_tables_are_unaffected() {
    let cs = chunks_of(
        "loose_struct.html",
        "<html><body><ul><li>one</li><li>two</li></ul>\
         <table><tr><td>a</td><td>b</td></tr></table></body></html>",
    );
    assert!(
        cs.iter().any(|c| c.content_type == "bullet_list"),
        "list lost"
    );
    assert!(cs.iter().any(|c| c.content_type == "table"), "table lost");
}

#[test]
fn an_empty_container_still_yields_nothing() {
    let cs = chunks_of(
        "loose_empty.html",
        "<html><body><div>   </div><div></div></body></html>",
    );
    assert!(cs.is_empty(), "whitespace produced chunks: {cs:?}");
}
