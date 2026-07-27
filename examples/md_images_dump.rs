//! Dumps to_markdown_with_images results for parity vs py-chunks
//! get_markdown(list_images=True). Line: {path, ext, ok, markdown, image_names, n_images}

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

type R = chunks_rs::Result<(String, Vec<(String, Vec<u8>)>)>;

fn call(ext: &str, p: &str) -> Option<R> {
    Some(match ext {
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown_with_images(p),
        "doc" => formats::doc::to_markdown_with_images(p),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown_with_images(p),
        "ppt" => formats::ppt::to_markdown_with_images(p),
        "xlsx" | "xlsm" | "xltx" | "xltm" | "xlsb" | "ods" => formats::xlsx::to_markdown_with_images(p),
        "html" | "htm" => formats::html::to_markdown_with_images(p),
        "pdf" => formats::pdf::to_markdown_with_images(p),
        "epub" => formats::epub::to_markdown_with_images(p),
        "eml" => formats::eml::to_markdown_with_images(p),
        "ipynb" => formats::ipynb::to_markdown_with_images(p),
        "odt" | "odp" => formats::odf::to_markdown_with_images(p),
        _ => return None,
    })
}

fn main() {
    let mut files = Vec::new();
    walk(&root(), &mut files);
    files.sort();
    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) { continue; }
        let p = path.to_str().unwrap();
        let Some(res) = call(&ext, p) else { continue };
        let line = match res {
            Ok((md, images)) => {
                let mut names: Vec<&String> = images.iter().map(|(n, _)| n).collect();
                names.sort();
                serde_json::json!({"path": p, "ext": ext, "ok": true, "markdown": md, "image_names": names, "n_images": images.len()})
            }
            Err(e) => serde_json::json!({"path": p, "ext": ext, "ok": false, "error": e.to_string()}),
        };
        println!("{}", serde_json::to_string(&line).unwrap());
    }
}
