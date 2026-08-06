# rs-chunks

> **Part of [chunk-engine](https://github.com/RanjanKudesia/chunk-engine)** — one Rust engine, three byte-identical SDKs ([py-chunks](https://pypi.org/project/py-chunks/) · [js-chunks](https://www.npmjs.com/package/js-chunks) · [rs-chunks](https://crates.io/crates/rs-chunks)).
> Full documentation, playground and benchmarks: **[chunkengine.dev](https://www.chunkengine.dev)**

[![crates.io](https://img.shields.io/crates/v/rs-chunks?style=flat-square&color=e8511e)](https://crates.io/crates/rs-chunks)
[![License](https://img.shields.io/badge/license-MIT-green?style=flat-square)](LICENSE)

**The chunk-engine reference engine** — a pure-Rust library that turns any of
**36 file extensions** (17 format families) into typed, structure-aware chunks
for RAG. No ML, no external services, no separate parser step.

`py-chunks` and `js-chunks` are bindings over this crate; it is the source of
truth for all chunking behavior.

## Install

```bash
cargo add rs-chunks
```

The library import name is **`chunks_rs`**.

## Quick start

```rust
use chunks_rs::{get_chunks, get_chunks_from_bytes, get_markdown};

// get_chunks(path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
let chunks = get_chunks("report.docx", "semantic", 3, 1, 3, 15)?;

for c in &chunks {
    println!("[{}] {}", c.content_type, c.content);
    // c.metadata is a serde_json::Value with format-specific provenance
}

// From bytes — the filename drives dispatch by extension
let chunks = get_chunks_from_bytes(&bytes, "report.docx", "default", 3, 1, 3, 15)?;

// One-shot Markdown conversion
let md = get_markdown("deck.pptx")?;
```

Every chunk is
`Chunk { content: String, content_type: String, metadata: serde_json::Value }`.

📖 **[Chunking modes](https://www.chunkengine.dev/docs/chunking-modes)** ·
**[Supported formats](https://www.chunkengine.dev/docs/supported-formats)** ·
**[Output schema](https://www.chunkengine.dev/docs/output-schema)** ·
**[Metadata reference](https://www.chunkengine.dev/docs/metadata-reference)**

## Per-format APIs

Each format family is available under `chunks_rs::formats::*`, exposing `chunk`,
`chunk_with_options`, `stream` (a native `Iterator`), `to_markdown`, and — where
applicable — `*_with_images` and `*_from_bytes` entry points. Use these when you
need format-specific parameters the dispatcher doesn't expose:

```rust
use chunks_rs::formats::{csv, pptx};

let chunks = csv::chunk("data.csv", "row", 10, 5, 1, true, None, "utf-8", true)?;

for c in csv::stream("data.csv", "row", 10, 5, 1, true, None, "utf-8", true)? {
    let c = c?;   // streaming yields Result<Chunk>
}

// (chunks, images) — images are (name, bytes) pairs
let (chunks, images) = pptx::chunk_with_images("deck.pptx", "default", 3, 1, 3, 15)?;
```

## Features

PDF parsing is always compiled in — it is pure Rust and builds for `wasm32`.
The default **`pdf-native`** feature adds only page *rasterisation* (via the
`liteparse` crate / PDFium), the fallback used when a scanned PDF has no
embedded page image to return. Disable default features for `wasm32`:

```toml
rs-chunks = { version = "0.6", default-features = false }
```

PDFs still parse without it; a text-less one reports that it has no text rather
than returning page renders.

## Parity

Validated against the `py-chunks` reference implementation over **every fixture ×
every mode** (`examples/parity_dump.rs` + `examples/parity_check.py`):

- **2204 / 2214 chunk comparisons byte-identical (99.5%)**
- **1056 / 1056** image extractions identical
- **273 / 273** markdown conversions identical

All OOXML, legacy binary (`.doc`/`.ppt`), OpenDocument, email, ebook and
delimited families are byte-identical. The remaining differences are confined to
`semantic`-mode `primary_merge_reason`, a tie-break the reference engine resolves
via randomized `HashMap` iteration order.

Streaming (`stream`) yields the same chunks as `chunk`. Adversarial inputs fail
with a clean `ChunkError` and never panic — panic-prone third-party parsers are
wrapped.

## Develop

```bash
cargo test
cargo run --release --example parity_dump    # parity harness
```

## License

MIT
