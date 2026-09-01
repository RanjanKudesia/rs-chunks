//! Chunking modes and the per-format options bag.
//!
//! [`ChunkOptions`] is consumed by the per-format `chunk_with_options` entry
//! points (e.g. `formats::docx::chunk_with_options`); its field defaults mirror
//! the keyword-argument defaults of the Python `get_chunks()`. The
//! source-agnostic dispatch layer (`dispatch::get_chunks` and friends) does
//! **not** take a `ChunkOptions` — it takes positional arguments (`mode: &str`,
//! `window_size`, `overlap`, `sentences_per_chunk`, `paragraphs_per_page`) to
//! match the Python entry point one-for-one. There is no dispatch-level
//! streaming API; streaming is per-format (`formats::<fmt>::stream`).

/// The chunking strategies across formats. Not every mode applies to every
/// format; each format validates and maps `mode` onto the strategies it
/// supports (e.g. spreadsheets use `Row`/`Table`/`Sheet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChunkMode {
    /// Format's natural default strategy.
    #[default]
    Default,
    Section,
    Semantic,
    Sentence,
    SlidingWindow,
    PageAware,
    Structural,
    // Spreadsheet / delimited strategies
    Row,
    Table,
    Sheet,
}

impl ChunkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkMode::Default => "default",
            ChunkMode::Section => "section",
            ChunkMode::Semantic => "semantic",
            ChunkMode::Sentence => "sentence",
            ChunkMode::SlidingWindow => "sliding_window",
            ChunkMode::PageAware => "page_aware",
            ChunkMode::Structural => "structural",
            ChunkMode::Row => "row",
            ChunkMode::Table => "table",
            ChunkMode::Sheet => "sheet",
        }
    }

    /// Parse a mode string. Inherent convenience that delegates to the
    /// [`std::str::FromStr`] impl but returns `Option` (kept for
    /// backwards-compatibility with existing callers).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<ChunkMode> {
        <ChunkMode as std::str::FromStr>::from_str(s).ok()
    }
}

impl std::str::FromStr for ChunkMode {
    type Err = crate::error::ChunkError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "default" => ChunkMode::Default,
            "section" => ChunkMode::Section,
            "semantic" => ChunkMode::Semantic,
            "sentence" => ChunkMode::Sentence,
            "sliding_window" => ChunkMode::SlidingWindow,
            "page_aware" => ChunkMode::PageAware,
            "structural" => ChunkMode::Structural,
            "row" => ChunkMode::Row,
            "table" => ChunkMode::Table,
            "sheet" => ChunkMode::Sheet,
            other => {
                return Err(crate::error::ChunkError::InvalidArg(format!(
                    "unknown chunk mode '{other}'"
                )))
            }
        })
    }
}

/// Unified options. Field defaults match the Python API defaults so ported
/// tests observe identical behaviour.
#[derive(Debug, Clone)]
pub struct ChunkOptions {
    pub mode: ChunkMode,
    pub window_size: usize,
    pub overlap: usize,
    pub sentences_per_chunk: usize,
    pub paragraphs_per_page: usize,
    // Delimited / spreadsheet knobs
    pub rows_per_chunk: usize,
    pub include_headers: bool,
    pub delimiter: Option<u8>,
    pub encoding: String,
    pub skip_empty_rows: bool,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            mode: ChunkMode::Default,
            window_size: 3,
            overlap: 1,
            sentences_per_chunk: 3,
            paragraphs_per_page: 15,
            rows_per_chunk: 10,
            include_headers: true,
            delimiter: None,
            encoding: "auto".to_string(),
            skip_empty_rows: true,
        }
    }
}

impl ChunkOptions {
    pub fn new(mode: ChunkMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    pub fn with_window(mut self, window_size: usize, overlap: usize) -> Self {
        self.window_size = window_size;
        self.overlap = overlap;
        self
    }
}

/// Reject out-of-range mode arguments *before* the document is parsed.
///
/// `docx`, `pptx`, `xlsx`, `csv`, `doc` and `ppt` each validate inline and
/// return [`ChunkError::InvalidArg`](crate::ChunkError::InvalidArg). The
/// markdown-pipeline formats (`md`, `txt`, `html`, and everything routed
/// through `formats::pipeline` — odf, eml, json, rtf, msg, ipynb, pdf, epub)
/// instead let their builders return `Result<_, String>` and lifted the whole
/// thing with `map_err(ChunkError::Parse)`, so "overlap must be less than
/// window_size" — an argument error, raised before a single byte is parsed —
/// reached callers tagged as a *parse failure*. js-chunks surfaced that as
/// `kind: "parse"` where csv/xlsx gave `"invalid-arg"`.
///
/// (py-chunks was never affected: it validates in its own Python layer and
/// raises `ValueError` before the engine is called at all.)
///
/// The messages are the canonical ones — the wording `docx` and the published
/// error-handling docs already use. The builders keep their own guards as a
/// backstop; this one simply runs first, with the right variant.
pub(crate) fn validate_mode_args(
    mode: &str,
    window_size: usize,
    overlap: usize,
    sentences_per_chunk: usize,
    paragraphs_per_page: usize,
) -> crate::Result<()> {
    let bad = |m: &str| Err(crate::ChunkError::InvalidArg(m.to_string()));
    match mode {
        "sliding_window" => {
            if window_size == 0 {
                return bad("window_size must be greater than 0");
            }
            if overlap >= window_size {
                return bad("overlap must be less than window_size");
            }
        }
        "sentence" if sentences_per_chunk == 0 => {
            return bad("sentences_per_chunk must be greater than 0");
        }
        "page_aware" if paragraphs_per_page == 0 => {
            return bad("paragraphs_per_page must be greater than 0");
        }
        _ => {}
    }
    Ok(())
}
