//! Dump PDF chunk contents as JSONL for ad-hoc parity spot-checks against the
//! bindings. (All three SDKs share this crate's pure-Rust PDF parser; the
//! historical liteparse-wasm host path this compared against is gone.)
use chunks_rs::get_chunks;
fn main() {
    let p = std::env::args().nth(1).expect("path");
    let mode = std::env::args().nth(2).unwrap_or_else(|| "default".into());
    let chunks = get_chunks(&p, &mode, 3, 1, 3, 15).expect("chunk");
    for c in &chunks {
        println!("{}", serde_json::to_string(&serde_json::json!({
            "content_type": c.content_type, "content": c.content,
        })).unwrap());
    }
    eprintln!("n_chunks={}", chunks.len());
}
