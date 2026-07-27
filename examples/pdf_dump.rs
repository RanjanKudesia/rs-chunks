//! Dump native PDF chunk contents for parity vs the chunks-js liteparse-wasm path.
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
