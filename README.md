# rs-chunks

> **Part of [chunk-engine](https://github.com/RanjanKudesia/chunk-engine)** — one Rust engine, three byte-identical SDKs ([py-chunks](https://pypi.org/project/py-chunks/) · [js-chunks](https://www.npmjs.com/package/js-chunks) · [rs-chunks](https://crates.io/crates/rs-chunks)).
> Docs, playground and benchmarks: **[chunkengine.dev](https://www.chunkengine.dev)** — and a ⭐ on the [hub repo](https://github.com/RanjanKudesia/chunk-engine) helps a lot.

Fast, high-fidelity **document chunking for RAG** — a pure-Rust engine covering
**36 file extensions** across 17 format families (Office, OpenDocument, PDF,
email, ebooks, notebooks, delimited text, and more).

`rs-chunks` is the standalone Rust edition of the `py-chunks` engine. It is
validated to produce **the same chunks as `py-chunks`** across the entire fixture
corpus (see [Parity](#parity)).

## Supported formats

| Family | Extensions |
|---|---|
| Word (OOXML) | `.docx` `.docm` `.dotx` `.dotm` |
| Word (legacy) | `.doc` |
| PowerPoint (OOXML) | `.pptx` `.potx` `.potm` `.ppsx` `.ppsm` |
| PowerPoint (legacy) | `.ppt` |
| Excel / spreadsheets | `.xlsx` `.xls` `.xlsm` `.xlsb` `.ods` `.xltx` `.xltm` |
| OpenDocument | `.odt` `.odp` |
| PDF | `.pdf` |
| Email | `.eml` `.mbox` `.msg` |
| Markup / text | `.html` `.htm` `.md` `.txt` |
| Delimited | `.csv` `.tsv` |
| Data | `.json` `.jsonl` `.ndjson` |
| Ebook / notebook / rich text | `.epub` `.ipynb` `.rtf` |

## Usage

```rust
use chunks_rs::{get_chunks, get_markdown};

// Source-agnostic dispatch by extension. `mode` selects the strategy;
// "default" is each format's natural strategy.
let chunks = get_chunks("report.docx", "default", 3, 1, 3, 15)?;
for c in &chunks {
    println!("[{}] {}", c.content_type, c.content);
    // c.metadata is a serde_json::Value with format-specific provenance
}

// One-shot Markdown conversion.
let md = get_markdown("deck.pptx")?;
```

Every chunk is a `Chunk { content: String, content_type: String, metadata: serde_json::Value }`.

### Chunking modes

Prose/binary document formats support: `default`, `section`, `semantic`,
`sentence`, `page_aware`, `sliding_window`. Spreadsheets support: `row`
(default), `table`, `sheet`, `semantic`, `page_aware`, `sliding_window`.
Delimited text supports: `row` (default), `sliding_window`, `page_aware`.

### Per-format APIs

Each family is also directly available under `chunks_rs::formats::*`, exposing
`chunk`, `chunk_with_options`, `stream` (a native `Iterator`), `to_markdown`, and
(where applicable) `*_with_images` entry points.

```rust
use chunks_rs::formats::csv;
let chunks = csv::chunk("data.csv", "row", 10, 5, 1, true, None, "utf-8", true)?;
for c in csv::stream("data.csv", "row", 10, 5, 1, true, None, "utf-8", true)? {
    let c = c?; // streaming yields Result<Chunk>
}
```

## Parity

`rs-chunks` is checked against the `py-chunks` reference engine over **every
fixture × every mode** (`examples/parity_dump.rs` + `examples/parity_check.py`):

- **2204 / 2214 comparisons identical (99.5%)**.
- All OOXML (Word/PowerPoint/Excel), legacy binary (`.doc`/`.ppt`), OpenDocument,
  email, ebook, and delimited families are **byte-identical**.
- The handful of differences are confined to `semantic`-mode
  `primary_merge_reason`, a tie-break the reference engine resolves via a
  randomized `HashMap` iteration order — i.e. **`py-chunks` itself is
  non-deterministic there**, and `rs-chunks` reproduces that behavior. The
  `merge_reasons` list (and all chunk content) is identical.

Image extraction is likewise validated against py-chunks:

- `chunk_with_images` vs `get_chunks(list_images=True)` — **1056 / 1056 identical**
  (chunk counts, image counts, image names, and image-chunk contents), including
  the OOXML and legacy-binary (`.doc`/`.ppt` ODRAW) formats.
- `to_markdown_with_images` vs `get_markdown(list_images=True)` — **273 / 273
  identical** (markdown string + image set) across every image-bearing format.

Streaming (`stream`) yields the same chunks as `chunk`/`get_chunks`, matching
py-chunks' `stream_chunks` (verified equal to its batch output).

Robustness: adversarial fixtures fail with a clean `ChunkError` and never panic
or abort (panic-prone third-party parsers are wrapped).

## License

MIT.
