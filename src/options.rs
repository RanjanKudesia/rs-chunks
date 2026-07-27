//! Chunking modes and the unified options bag used by the dispatch layer.
//!
//! Per-format modules also expose faithful, format-specific entry points; this
//! `ChunkOptions` is the single knob-set the source-agnostic `get_chunks` /
//! `stream_chunks` dispatch routes through, mirroring the keyword arguments the
//! Python `get_chunks()` accepted.

/// The chunking strategies across formats. Not every mode applies to every
/// format; each format validates and maps `mode` onto the strategies it
/// supports (e.g. spreadsheets use `Row`/`Table`/`Sheet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMode {
    /// Format's natural default strategy.
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

    pub fn from_str(s: &str) -> Option<ChunkMode> {
        Some(match s {
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
            _ => return None,
        })
    }
}

impl Default for ChunkMode {
    fn default() -> Self {
        ChunkMode::Default
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
            encoding: "utf-8".to_string(),
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
