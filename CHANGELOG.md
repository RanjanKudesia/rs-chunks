# Changelog

All notable changes to `rs-chunks` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`rs-chunks` is the engine; `py-chunks` (PyPI) and `js-chunks` (npm) are thin
bindings over it and share its version. Anything here that changes chunk
`content`, `content_type` or `metadata` changes all three.

Started 2026-08-16 — this crate had no changelog while its two bindings did.
Earlier history is in the git log and in the workspace's `TECH_DEBT.md`.

## [Unreleased]

## [0.6.3] - 2026-08-18

### Changed

- **Empty documents return `[]` instead of an error** for `.txt`, `.html` and
  `.md`, matching what `.docx`/`.ppt`/`.xlsx` always did. Whether "no content"
  was an error used to depend on the file extension. The rule is now one line:
  *parsed fine but nothing to chunk → `[]`; structurally invalid → typed error
  carrying a remedy* (a PDF with no text layer still says to pass `list_images`).
  24 raise-sites across `formats/{txt,html,md,pptx}` were converted.
- **CSV/TSV decode auto-detects the encoding**, via a new `"auto"` label that is
  now the default. UTF-8 is attempted **strictly first**, so any file that
  already decoded is byte-identical; detection only runs where the old code
  returned an error. The same latin-1 bytes now read as `.csv` exactly as they
  always did as `.txt`.
- **CommonMark autolinks survive chunking.** `<user@host>` and `<scheme:…>` are
  autolinks, not raw inline HTML; they were being discarded along with real
  tags, which deleted **254 email addresses** from chunk content across
  `.eml`/`.mbox`/`.msg`. Raw HTML is still stripped, and `.md` keeps exact
  CommonMark behaviour. `get_markdown` was never affected and is unchanged.
- Spreadsheet `semantic` mode now respects the documented 1,500-character cap,
  splitting at row boundaries. A single row wider than the cap is indivisible and
  is emitted whole.
- The XLSX invalid-mode message lists `default`, which the dispatch arm
  (`"row" | "default"`) has always accepted.

### Fixed

- **HTML that is not UTF-8 now decodes instead of failing or mangling.** The
  same document was read three different ways: `fs::read_to_string` at two entry
  points, which **errors** on non-UTF-8, and `String::from_utf8_lossy` at six
  others, which silently replaces every non-ASCII byte with `U+FFFD`. So a
  `windows-1251` page errored, produced mojibake, or worked depending on which
  function you called.

  A new `formats/html/encoding.rs` decodes once for every path: BOM, then valid
  UTF-8, then the document's own `<meta charset>` (via `encoding_rs`, so any
  WHATWG label works), then detection. **Valid UTF-8 deliberately outranks the
  declaration** — contrary to the WHATWG order — so no document that already
  decoded can change. Found by the neutral x86 full-corpus run: these were 2 of
  only 3 legitimate documents in 638 the engine could not read, and both
  competitors read them.


- **A malformed `.xls` could exhaust memory and get the process OOM-killed.**
  Nine of Apache POI's fifteen fuzzer fixtures allocated past 2 GB in ~0.3 s from
  inputs as small as 1,782 bytes. The allocation happened inside the spreadsheet
  reader *before* any panic, so `catch_unwind` could not intercept it — an OOM
  kill takes the host process down and no caller can defend against it. Fixed by
  upgrading calamine 0.26 → 0.35. Guarded by `tests/xls_allocation_bound.rs`.
- That upgrade also corrected four parsing defects: a declared custom number
  format is now applied, stray carriage returns are normalised out of cell text,
  an ODS column offset is fixed, and one XLSB file that previously failed to open
  now parses.
- **DOCX `list_images=true` no longer drops paragraph prose** when images are
  anchored outside any heading. Text is now identical with and without image
  extraction.
- **PPTX `get_markdown` no longer spaces out resolved entities** — `AT&amp;T`
  rendered as `AT & T` because an entity reference splits one `<a:t>` into
  several XML events which were then space-joined.
- **EPUB no longer swallows per-chapter parse failures.** A real failure now
  propagates with its spine index and href instead of silently yielding a
  shorter book.
- `formats/ppt/cfb_reader.rs` opens the compound file once via `PptCfb` rather
  than re-parsing the whole CFB directory tree for every image-bearing deck.
