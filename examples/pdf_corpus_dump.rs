//! Dump every PDF fixture's markdown and image inventory to a directory, so a
//! parser change can be read line by line instead of judged by counts.
//!
//! The golden snapshot records a sha and a chunk count; it cannot tell you
//! *what* moved. Capture this before a PDF change and after it, then diff the
//! two directories — that is how the parser rewrite's 154 snapshot diffs were
//! each accounted for.
//!
//! Usage: cargo run --release --example pdf_corpus_dump -- <out_dir>
use std::fs;
use std::path::Path;

fn main() {
    let out = std::env::args().nth(1).expect("out dir");
    fs::create_dir_all(&out).expect("mkdir");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test_files/pdf");
    let mut names: Vec<_> = fs::read_dir(&dir)
        .expect("fixtures")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.to_ascii_lowercase().ends_with(".pdf"))
        .collect();
    names.sort();

    let mut index = String::new();
    for name in &names {
        let path = dir.join(name).to_string_lossy().to_string();
        match chunks_rs::formats::pdf::to_markdown_with_images(&path) {
            Ok((md, images)) => {
                fs::write(Path::new(&out).join(format!("{name}.md")), &md).unwrap();
                let mut inv: Vec<String> = images
                    .iter()
                    .map(|(n, b)| format!("{n}\t{}\t{:016x}", b.len(), fnv(b)))
                    .collect();
                inv.sort();
                fs::write(
                    Path::new(&out).join(format!("{name}.images")),
                    inv.join("\n"),
                )
                .unwrap();
                index.push_str(&format!(
                    "{name}\tOK\tmd_chars={}\timages={}\n",
                    md.chars().count(),
                    images.len()
                ));
            }
            Err(e) => {
                fs::write(Path::new(&out).join(format!("{name}.err")), e.to_string()).unwrap();
                index.push_str(&format!("{name}\tERR\t{e}\n"));
            }
        }
    }
    fs::write(Path::new(&out).join("_index.tsv"), index).unwrap();
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}
