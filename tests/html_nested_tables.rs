//! TECH_DEBT #24: a nested `<table>` must not dissolve into its parent.
//!
//! No fixture in the corpus contains nested tables — the tracker entry said so
//! and asked for one before prioritising. That absence is a fixture gap, not
//! evidence of correctness: nested tables are ordinary in real-world HTML
//! (layout tables, HTML email), and the failure was total, so the repro is
//! constructed here deliberately and labelled as such.

use chunks_rs::formats::html;

fn chunk_html(body: &str) -> Vec<String> {
    // A counter, not the body's length: two of these tests chunk the same
    // `NESTED` constant, so keying on length gave them one shared path that
    // they raced to write and read — an intermittent "HTML file is empty
    // after decoding" that had nothing to do with nested tables.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "chunks_rs_nested_{}_{}.html",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, format!("<html><body>{body}</body></html>")).unwrap();
    html::chunk(path.to_str().unwrap(), "structural", 3, 1, 3, 15)
        .unwrap()
        .into_iter()
        .map(|c| c.content)
        .collect()
}

const NESTED: &str = "<table>\
  <tr><td>Outer A1</td><td>Outer B1</td></tr>\
  <tr><td><table>\
     <tr><td>Inner X</td><td>Inner Y</td></tr>\
     <tr><td>Inner P</td><td>Inner Q</td></tr>\
  </table></td><td>Outer B2</td></tr>\
</table>";

/// The parent keeps exactly its own rows: `<tr>` scanning ran at any depth, so
/// the inner table's two rows were appended to the parent as rows of its own.
#[test]
fn nested_rows_do_not_leak_into_the_parent() {
    let chunks = chunk_html(NESTED);
    let table = chunks
        .iter()
        .find(|c| c.contains("Outer A1"))
        .expect("a table chunk");

    assert_eq!(
        table.lines().count(),
        2,
        "the outer table has two rows; got:\n{table}"
    );
    assert!(
        table
            .lines()
            .next()
            .unwrap()
            .starts_with("Outer A1 | Outer B1"),
        "first row wrong: {table}"
    );
}

/// The inner cells keep their separator. `strip_tags` breaks on `br|p|li|tr`
/// but not `td|th`, so a nested table flattened to "Inner XInner Y" — two
/// distinct cells run together into one meaningless token.
#[test]
fn nested_cells_keep_their_separator() {
    let chunks = chunk_html(NESTED);
    let table = chunks
        .iter()
        .find(|c| c.contains("Outer A1"))
        .expect("a table chunk");

    assert!(
        !table.contains("Inner XInner Y"),
        "nested cells were run together: {table}"
    );
    assert!(
        table.contains("Inner X | Inner Y"),
        "nested cells lost their separator: {table}"
    );
    // The cell that holds the nested table still carries its sibling.
    assert!(
        table.contains("Outer B2"),
        "the parent row lost its other cell: {table}"
    );
}

/// A table with no nesting is unaffected — this is the shape the whole corpus
/// uses, and it must render exactly as before.
#[test]
fn a_flat_table_is_unchanged() {
    let chunks = chunk_html(
        "<table><tr><th>H1</th><th>H2</th></tr>\
         <tr><td>a</td><td>b</td></tr>\
         <tr><td>c</td><td>d</td></tr></table>",
    );
    let table = chunks.iter().find(|c| c.contains("H1")).expect("a table");
    assert_eq!(
        table.lines().collect::<Vec<_>>(),
        vec!["H1 | H2", "a | b", "c | d"],
        "flat table rendering changed"
    );
}

/// Self-closing empty cells must not swallow the cells after them.
///
/// Caught as a regression this change introduced: making row/cell extraction
/// depth-aware added a depth counter that treated `<td/>` as *opening* a cell,
/// so Moby Dick's `<td/><td>word</td><td>lang</td>` rows — Project Gutenberg
/// uses an empty leading cell for indentation — collapsed from 13 two-cell rows
/// into 26 one-cell rows. Real XHTML does this constantly.
#[test]
fn self_closing_empty_cells_do_not_swallow_the_row() {
    let chunks = chunk_html(
        "<table><tbody>\
         <tr><td/><td>word,</td><td>Language.</td></tr>\
         <tr><td/><td>other,</td><td>Tongue.</td></tr>\
         </tbody></table>",
    );
    let table = chunks
        .iter()
        .find(|c| c.contains("word,"))
        .expect("a table chunk");

    assert_eq!(
        table.lines().collect::<Vec<_>>(),
        vec!["word, | Language.", "other, | Tongue."],
        "self-closing cells broke the row structure:\n{table}"
    );
}
