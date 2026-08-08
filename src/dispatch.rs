//! Source-agnostic dispatch: route a file to the right engine by extension,
//! mirroring the Python `get_chunks()` routing (including the delimited/
//! spreadsheet special-casing) so behaviour matches the reference package.
//!
//! Every public entry point here runs the parse behind a `catch_unwind`
//! boundary: a panic anywhere in the engine (or a third-party parser) is
//! converted into [`ChunkError::Parse`] instead of unwinding into the caller.

use std::path::Path;

use crate::chunk::Chunk;
use crate::error::{ChunkError, Result};
use crate::formats;

/// Run `f` behind a panic boundary, converting any panic into
/// [`ChunkError::Parse`] so adversarial inputs can never unwind across the
/// public dispatch API.
///
/// `AssertUnwindSafe` is justified: the closure only captures shared references
/// to caller-owned input (`&[u8]` / `&str`) plus `Copy` scalars, the engine
/// keeps no global mutable state, and on the panic path every partially-built
/// value is owned by the closure and dropped — nothing observable is left in a
/// broken state.
fn catch_parser_panics<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_string());
            Err(ChunkError::Parse(format!("internal parser panic: {msg}")))
        }
    }
}

/// Chunk a document supplied as raw bytes. `filename` is used only for extension
/// detection (routing) — never persisted under that name.
///
/// Routes to each format's no-filesystem `chunk_from_bytes`; unsupported
/// extensions return [`ChunkError::Unsupported`]. See [`get_chunks`] for the
/// `sentences_per_chunk == 3` spreadsheet sentinel.
pub fn get_chunks_from_bytes(
    data: &[u8],
    filename: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    catch_parser_panics(|| {
        get_chunks_from_bytes_inner(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
    })
}

fn get_chunks_from_bytes_inner(
    data: &[u8],
    filename: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "csv" | "tsv" => {
            let csv_mode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if csv_mode == "page_aware" { paragraphs_per_page } else { csv_rows_per_chunk(sentences_per_chunk) };
            let delimiter = if ext == "tsv" { Some(b'\t') } else { None };
            formats::csv::chunk_from_bytes(data, csv_mode, rows_per_chunk, window_size, overlap, true, delimiter, "utf-8", true)
        }
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => {
            let xmode = if mode == "default" { "row" } else { mode };
            check_spreadsheet_page_arg(xmode, paragraphs_per_page)?;
            // Sentinel (parity with the Python default): 3 == "caller left the
            // default", mapped to rows_per_chunk = 1. See `get_chunks`.
            let rows_per_chunk = if sentences_per_chunk == 3 { 1 } else { sentences_per_chunk };
            formats::xlsx::chunk_from_bytes(data, &ext, xmode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000)
        }
        "md" => formats::md::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "txt" => formats::txt::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "html" | "htm" => formats::html::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "json" | "jsonl" | "ndjson" => formats::json::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "eml" | "mbox" => formats::eml::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "odt" | "odp" => formats::odf::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ipynb" => formats::ipynb::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "rtf" => formats::rtf::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "epub" => formats::epub::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "msg" => formats::msg::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "doc" => formats::doc::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ppt" => formats::ppt::chunk_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pdf" => formats::pdf::chunk_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        other => Err(ChunkError::Unsupported(format!("Unsupported file type '.{other}'"))),
    }
}

/// Convert bytes to Markdown; see [`get_chunks_from_bytes`] for the routing note.
pub fn get_markdown_from_bytes(data: &[u8], filename: &str) -> Result<String> {
    catch_parser_panics(|| get_markdown_from_bytes_inner(data, filename))
}

fn get_markdown_from_bytes_inner(data: &[u8], filename: &str) -> Result<String> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "csv" => formats::csv::to_markdown_from_bytes(data, None, "utf-8"),
        "tsv" => formats::csv::to_markdown_from_bytes(data, Some(b'\t'), "utf-8"),
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => formats::xlsx::to_markdown_from_bytes(data, &ext),
        "md" => formats::md::to_markdown_from_bytes(data),
        "txt" => formats::txt::to_markdown_from_bytes(data),
        "html" | "htm" => formats::html::to_markdown_from_bytes(data),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown_from_bytes(data),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown_from_bytes(data),
        "json" | "jsonl" | "ndjson" => formats::json::to_markdown_from_bytes(data, filename),
        "eml" | "mbox" => formats::eml::to_markdown_from_bytes(data, filename),
        "odt" | "odp" => formats::odf::to_markdown_from_bytes(data, filename),
        "ipynb" => formats::ipynb::to_markdown_from_bytes(data),
        "rtf" => formats::rtf::to_markdown_from_bytes(data),
        "epub" => formats::epub::to_markdown_from_bytes(data),
        "msg" => formats::msg::to_markdown_from_bytes(data),
        "doc" => formats::doc::to_markdown_from_bytes(data),
        "ppt" => formats::ppt::to_markdown_from_bytes(data),
        "pdf" => formats::pdf::to_markdown_from_bytes(data),
        other => Err(ChunkError::Unsupported(format!("get_markdown does not support '.{other}'"))),
    }
}

/// Chunk bytes and also return extracted image bytes (`list_images=True`).
/// Formats without embedded-image support return an empty image list and the
/// same chunks as [`get_chunks_from_bytes`].
#[allow(clippy::too_many_arguments)]
pub fn get_chunks_with_images_from_bytes(
    data: &[u8],
    filename: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    catch_parser_panics(|| {
        get_chunks_with_images_from_bytes_inner(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
    })
}

#[allow(clippy::too_many_arguments)]
fn get_chunks_with_images_from_bytes_inner(
    data: &[u8],
    filename: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<(Vec<Chunk>, Vec<(String, Vec<u8>)>)> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => {
            let xmode = if mode == "default" { "row" } else { mode };
            check_spreadsheet_page_arg(xmode, paragraphs_per_page)?;
            // Sentinel (parity with the Python default): 3 == "caller left the
            // default", mapped to rows_per_chunk = 1. See `get_chunks`.
            let rows_per_chunk = if sentences_per_chunk == 3 { 1 } else { sentences_per_chunk };
            formats::xlsx::chunk_with_images_from_bytes(data, &ext, xmode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000)
        }
        "html" | "htm" => formats::html::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "eml" | "mbox" => formats::eml::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "msg" => formats::msg::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "odt" | "odp" => formats::odf::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ipynb" => formats::ipynb::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "epub" => formats::epub::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "doc" => formats::doc::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ppt" => formats::ppt::chunk_with_images_from_bytes(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pdf" => formats::pdf::chunk_with_images_from_bytes(data, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        // No embedded-image support: chunks only, empty image list.
        _ => Ok((get_chunks_from_bytes_inner(data, filename, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)?, Vec::new())),
    }
}

/// Convert bytes to Markdown and return extracted image bytes (`list_images=True`).
pub fn get_markdown_with_images_from_bytes(data: &[u8], filename: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    catch_parser_panics(|| get_markdown_with_images_from_bytes_inner(data, filename))
}

fn get_markdown_with_images_from_bytes_inner(data: &[u8], filename: &str) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let ext = ext_of(filename);
    match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => formats::xlsx::to_markdown_with_images_from_bytes(data, &ext),
        "html" | "htm" => formats::html::to_markdown_with_images_from_bytes(data),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown_with_images_from_bytes(data),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown_with_images_from_bytes(data),
        "eml" | "mbox" => formats::eml::to_markdown_with_images_from_bytes(data, filename),
        "msg" => formats::msg::to_markdown_with_images_from_bytes(data),
        "odt" | "odp" => formats::odf::to_markdown_with_images_from_bytes(data, filename),
        "ipynb" => formats::ipynb::to_markdown_with_images_from_bytes(data),
        "epub" => formats::epub::to_markdown_with_images_from_bytes(data),
        "doc" => formats::doc::to_markdown_with_images_from_bytes(data),
        "ppt" => formats::ppt::to_markdown_with_images_from_bytes(data),
        "pdf" => formats::pdf::to_markdown_with_images_from_bytes(data),
        _ => Ok((get_markdown_from_bytes_inner(data, filename)?, Vec::new())),
    }
}

fn ext_of(file_path: &str) -> String {
    Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn csv_rows_per_chunk(sentences_per_chunk: usize) -> usize {
    sentences_per_chunk.max(1)
}

/// Reject `paragraphs_per_page == 0` for the spreadsheet family.
///
/// Spreadsheets paginate by `rows_per_chunk`, so the arms below map
/// `sentences_per_chunk` and drop `paragraphs_per_page` entirely — which meant
/// `page_aware` with `0` was silently accepted here while every other format
/// rejects it, and three docs pages promise it is rejected. The parameter stays
/// *ignored* (a spreadsheet page is a sheet region, not a unit count); only an
/// unusable value is now an error.
///
/// Mode-scoped exactly like [`crate::options::validate_mode_args`]: a value is
/// only required to be usable by the mode that reads it, so `0` in `row`/
/// `sheet`/`table` remains fine — as it is for every other format.
fn check_spreadsheet_page_arg(mode: &str, paragraphs_per_page: usize) -> Result<()> {
    if mode == "page_aware" && paragraphs_per_page == 0 {
        return Err(ChunkError::InvalidArg(
            "paragraphs_per_page must be greater than 0".to_string(),
        ));
    }
    Ok(())
}

/// Chunk any supported document by path. `mode` is passed through to the engine
/// ("default" selects each format's natural strategy); the delimited formats map
/// it onto their row/window/page strategies exactly like the Python entry point.
///
/// # The `sentences_per_chunk == 3` spreadsheet sentinel
///
/// For spreadsheet extensions (`xlsx`/`xls`/`xlsm`/`xlsb`/`ods`/`xltx`/`xltm`)
/// the value `3` — the Python API's *default* for `sentences_per_chunk` — is
/// treated as "caller didn't ask" and mapped to `rows_per_chunk = 1`, the
/// spreadsheet default. This mirrors the reference Python `get_chunks()`
/// exactly and is a deliberate parity constraint. The consequence: a caller
/// who *deliberately* wants 3 rows per chunk cannot express it through this
/// entry point (3 is unreachable); use `formats::xlsx::chunk` /
/// `chunk_with_options` directly, which take `rows_per_chunk` verbatim.
pub fn get_chunks(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    catch_parser_panics(|| {
        get_chunks_inner(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
    })
}

fn get_chunks_inner(
    file_path: &str,
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> Result<Vec<Chunk>> {
    let ext = ext_of(file_path);
    match ext.as_str() {
        // ── Delimited text ──────────────────────────────────────────────
        "csv" | "tsv" => {
            let csv_mode = if mode == "default" { "row" } else { mode };
            let rows_per_chunk = if csv_mode == "page_aware" {
                paragraphs_per_page
            } else {
                csv_rows_per_chunk(sentences_per_chunk)
            };
            let delimiter = if ext == "tsv" { Some(b'\t') } else { None };
            formats::csv::chunk(
                file_path,
                csv_mode,
                rows_per_chunk,
                window_size,
                overlap,
                true,
                delimiter,
                "utf-8",
                true,
            )
        }
        // ── Spreadsheets (calamine) ─────────────────────────────────────
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => {
            let xmode = if mode == "default" { "row" } else { mode };
            check_spreadsheet_page_arg(xmode, paragraphs_per_page)?;
            // Sentinel (parity with the Python default): 3 == "caller left the
            // default", mapped to rows_per_chunk = 1. A deliberate 3 is
            // unreachable here — see the doc comment on `get_chunks`.
            let rows_per_chunk = if sentences_per_chunk == 3 { 1 } else { sentences_per_chunk };
            formats::xlsx::chunk(
                file_path, xmode, rows_per_chunk, window_size, overlap, true, Vec::new(), true, 2000,
            )
        }
        "doc" => formats::doc::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        // ── Word OOXML ──────────────────────────────────────────────────
        "docx" | "docm" | "dotx" | "dotm" => {
            formats::docx::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
        }
        // ── Prose / markdown-pipeline formats ───────────────────────────
        "ppt" => formats::ppt::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => {
            formats::pptx::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page)
        }
        "md" => formats::md::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "txt" => formats::txt::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "html" | "htm" => formats::html::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "json" | "jsonl" | "ndjson" => formats::json::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "eml" | "mbox" => formats::eml::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "odt" | "odp" => formats::odf::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "msg" => formats::msg::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "ipynb" => formats::ipynb::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "rtf" => formats::rtf::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "pdf" => formats::pdf::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        "epub" => formats::epub::chunk(file_path, mode, window_size, overlap, sentences_per_chunk, paragraphs_per_page),
        other => Err(ChunkError::Unsupported(format!("Unsupported file type '.{other}'"))),
    }
}

/// Convert a supported document to Markdown by path.
pub fn get_markdown(file_path: &str) -> Result<String> {
    catch_parser_panics(|| get_markdown_inner(file_path))
}

fn get_markdown_inner(file_path: &str) -> Result<String> {
    let ext = ext_of(file_path);
    match ext.as_str() {
        "csv" | "tsv" => {
            let delimiter = if ext == "tsv" { Some(b'\t') } else { None };
            formats::csv::to_markdown(file_path, delimiter, "utf-8")
        }
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "xltx" | "xltm" => formats::xlsx::to_markdown(file_path),
        "doc" => formats::doc::to_markdown(file_path),
        "docx" | "docm" | "dotx" | "dotm" => formats::docx::to_markdown(file_path),
        "ppt" => formats::ppt::to_markdown(file_path),
        "pptx" | "potx" | "potm" | "ppsx" | "ppsm" => formats::pptx::to_markdown(file_path),
        "md" => formats::md::to_markdown(file_path),
        "txt" => formats::txt::to_markdown(file_path),
        "html" | "htm" => formats::html::to_markdown(file_path),
        "json" | "jsonl" | "ndjson" => formats::json::to_markdown(file_path),
        "eml" | "mbox" => formats::eml::to_markdown(file_path),
        "odt" | "odp" => formats::odf::to_markdown(file_path),
        "msg" => formats::msg::to_markdown(file_path),
        "ipynb" => formats::ipynb::to_markdown(file_path),
        "rtf" => formats::rtf::to_markdown(file_path),
        "pdf" => formats::pdf::to_markdown(file_path),
        "epub" => formats::epub::to_markdown(file_path),
        other => Err(ChunkError::Unsupported(format!("get_markdown does not support '.{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_boundary_converts_panics_to_parse_errors() {
        let err = catch_parser_panics::<()>(|| panic!("boom at offset 42")).unwrap_err();
        match err {
            ChunkError::Parse(m) => {
                assert!(m.contains("internal parser panic"), "unexpected message: {m}");
                assert!(m.contains("boom at offset 42"), "payload lost: {m}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn panic_boundary_passes_results_through() {
        assert!(catch_parser_panics(|| Ok(7)).is_ok_and(|v| v == 7));
        assert!(matches!(
            catch_parser_panics::<()>(|| Err(ChunkError::InvalidArg("x".into()))),
            Err(ChunkError::InvalidArg(_))
        ));
    }
}
