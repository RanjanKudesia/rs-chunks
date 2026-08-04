//! Print the extracted markdown + recovered metadata for each `.rtf` argument.
//!
//! Ad-hoc inspection aid for the RTF extractor — `verify_output.py` is the
//! pinned ground truth.

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                println!("=== {path} — unreadable: {e}");
                continue;
            }
        };
        let doc = chunks_rs::formats::rtf::extract::extract(&bytes);
        println!("=== {path}");
        println!("  title:  {:?}", doc.title);
        println!("  author: {:?}", doc.author);
        let md = chunks_rs::formats::rtf::extract::to_markdown(&doc);
        for (i, line) in md.split('\n').enumerate() {
            println!("  {i:3} {line:?}");
        }
        println!();
    }
}
