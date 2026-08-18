//! DOCX structure defects: section ordering, list grouping, image-adjacent text.
//!
//! All three reproduce on `all_round.docx`, an accessibility-demo document with
//! nested headings, a bulleted outline, and paragraphs that carry an inline
//! image alongside their body text — the three shapes TECH_DEBT #1, #3 and #2
//! each got wrong.

use std::path::{Path, PathBuf};

use chunks_rs::formats::docx;

fn fixture() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join("docx")
        .join("all_round.docx");
    assert!(p.is_file(), "missing fixture: {}", p.display());
    p
}

fn contents(mode: &str) -> Vec<String> {
    docx::chunk(fixture().to_str().unwrap(), mode, 3, 1, 3, 15)
        .unwrap_or_else(|e| panic!("{mode}: {e:?}"))
        .into_iter()
        .map(|c| c.content)
        .collect()
}

/// #1: `section` emitted chunks in the order sections *closed*, not the order
/// they appear.
///
/// Sections close inside-out — a subsection pops before its parent, and the
/// outermost not until EOF — so the document's own title came back seventh of
/// nine, and level-3 subsections preceded the level-2 heading they sit under.
/// The order here is read off `structural`, which walks the document linearly.
#[test]
fn section_chunks_follow_reading_order() {
    let structural = contents("structural");
    let section = contents("section");

    let first_line = |s: &String| s.lines().next().unwrap_or("").to_string();
    let doc_order: Vec<String> = structural.iter().map(first_line).collect();
    let section_order: Vec<String> = section.iter().map(first_line).collect();

    // Every section heading must appear in the same relative order as the
    // linear walk of the document.
    let positions: Vec<usize> = section_order
        .iter()
        .filter_map(|h| doc_order.iter().position(|d| d == h))
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] <= w[1]),
        "section order does not follow the document:\n  section:    {section_order:?}\n  structural: {doc_order:?}"
    );

    assert_eq!(
        section_order.first().map(String::as_str),
        Some("Sample Document"),
        "the document's own title must be the first section chunk, not the seventh"
    );
}

/// #3: `semantic` chopped a bulleted list into three-item pieces.
///
/// `can_short_merge` refuses to grow a chunk past three consecutive short
/// paragraphs — right for prose, wrong for a list, where every item is short by
/// construction. The eight-item outline in this fixture came back as three
/// chunks of 27–35 characters.
#[test]
fn semantic_keeps_a_bulleted_list_together() {
    let semantic = contents("semantic");

    let list_chunks: Vec<&String> = semantic
        .iter()
        .filter(|c| {
            c.lines()
                .filter(|l| l.trim_start().starts_with("- "))
                .count()
                >= 2
        })
        .collect();

    assert_eq!(
        list_chunks.len(),
        1,
        "the outline should be one chunk, got {}: {:?}",
        list_chunks.len(),
        list_chunks
    );

    let list = list_chunks[0];
    for item in ["Headings", "Lists", "Links", "Images", "Tables", "Columns"] {
        assert!(
            list.contains(item),
            "list chunk is missing {item:?}: {list:?}"
        );
    }
}

/// #2: `get_markdown` dropped the body text of any paragraph holding an inline
/// image, emitting only the `[Image: …]` marker.
///
/// `get_chunks` had the mirror-image bug — it kept the text and dropped the
/// marker — so the two representations of the same paragraph each lost
/// something different. Markdown must carry both.
#[test]
fn markdown_keeps_text_that_shares_a_paragraph_with_an_image() {
    let md = docx::to_markdown(fixture().to_str().unwrap()).expect("markdown");

    let sentence = "there is an image of the web accessibility symbol";
    assert!(
        md.contains(sentence),
        "markdown dropped the text of an image-bearing paragraph: {sentence:?}"
    );
    assert!(
        md.contains("[Image: Web Access Symbol]"),
        "markdown dropped the image marker it used to keep"
    );

    // The text must come before its image, in document order.
    let t = md.find(sentence).expect("text present");
    let i = md
        .find("[Image: Web Access Symbol]")
        .expect("marker present");
    assert!(t < i, "text should precede the image marker it introduces");

    // The second image-bearing paragraph too — one fix, not a special case.
    assert!(
        md.contains("Some images, such as charts or graphs, require long descriptions"),
        "markdown dropped the second image-bearing paragraph"
    );
}

/// The chunk text is what `get_chunks` always returned; #2's markdown fix must
/// not have disturbed it.
#[test]
fn image_adjacent_text_still_reaches_the_chunks() {
    let structural = contents("structural");
    assert!(
        structural
            .iter()
            .any(|c| c.contains("there is an image of the web accessibility symbol")),
        "image-adjacent body text vanished from the chunks"
    );
}
