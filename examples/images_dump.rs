//! Dumps chunk_with_images results for formats with image support, for parity
//! checking against py-chunks get_chunks(list_images=True).
//! Line: {"path","ext","mode","ok","n_chunks","n_images","image_names":[...],
//!        "image_chunk_contents":[...]}

use std::path::PathBuf;

use chunks_rs::formats;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("test_files")
}
fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk(&p, out); } else { out.push(p); }
        }
    }
}

type ImgResult = chunks_rs::Result<(Vec<chunks_rs::Chunk>, Vec<(String, Vec<u8>)>)>;

fn call(ext: &str, p: &str, mode: &str) -> Option<ImgResult> {
    let r = match ext {
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::chunk_with_images(p, mode, 3, 1, 3, 15),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::chunk_with_images(p, mode, 3, 1, 3, 15),
        "xlsx" | "xlsm" | "xltx" | "xltm" | "xlsb" | "ods" => {
            formats::xlsx::chunk_with_images(p, mode, 1, 3, 1, true, Vec::new(), true, 2000)
        }
        "doc" => formats::doc::chunk_with_images(p, mode, 3, 1, 3, 15),
        "ppt" => formats::ppt::chunk_with_images(p, mode, 3, 1, 3, 15),
        _ => return None,
    };
    Some(r)
}

const IMG_EXTS: &[&str] = &[
    "docx", "docm", "dotx", "dotm", "doc", "pptx", "potx", "potm", "ppsx", "ppsm", "ppt", "xlsx",
    "xlsm", "xltx", "xltm", "xlsb", "ods",
];
const MODES: &[&str] = &["default", "section", "semantic", "sentence", "page_aware", "sliding_window"];

fn main() {
    let mut files = Vec::new();
    walk(&root(), &mut files);
    files.sort();
    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        if !IMG_EXTS.contains(&ext.as_str()) { continue; }
        if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) { continue; }
        let p = path.to_str().unwrap();
        let modes: &[&str] = if matches!(ext.as_str(), "xlsx" | "xlsm" | "xltx" | "xltm" | "xlsb" | "ods") {
            &["default", "table", "sheet", "semantic", "page_aware", "sliding_window"]
        } else {
            MODES
        };
        for mode in modes {
            let Some(res) = call(&ext, p, mode) else { continue };
            match res {
                Ok((chunks, images)) => {
                    let mut names: Vec<&String> = images.iter().map(|(n, _)| n).collect();
                    names.sort();
                    let img_chunk_contents: Vec<&str> = chunks.iter()
                        .filter(|c| c.content_type == "image").map(|c| c.content.as_str()).collect();
                    let line = serde_json::json!({
                        "path": p, "ext": ext, "mode": mode, "ok": true,
                        "n_chunks": chunks.len(), "n_images": images.len(),
                        "image_names": names, "image_chunk_contents": img_chunk_contents,
                    });
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
                Err(e) => {
                    let line = serde_json::json!({"path": p, "ext": ext, "mode": mode, "ok": false, "error": e.to_string()});
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
        }
    }
}
