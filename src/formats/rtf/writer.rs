//! Emphasis-aware markdown writer for the RTF tokenizer.
//!
//! RTF marks formatting as state (`\b` on, `\b0` off) rather than as spans, so
//! the writer tracks what is currently *open* in the output and reconciles it
//! whenever the incoming run's formatting differs. It also owns the "line prefix"
//! used for headings and list markers, so those are emitted exactly once, at a
//! line start, and dropped again if the paragraph turns out to be empty.

/// Character formatting carried by a run.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Fmt {
    pub bold: bool,
    pub italic: bool,
}

impl Fmt {
    pub const NONE: Fmt = Fmt {
        bold: false,
        italic: false,
    };

    fn is_none(self) -> bool {
        self == Fmt::NONE
    }

    fn marker_len(self) -> usize {
        usize::from(self.bold) * 2 + usize::from(self.italic)
    }
}

#[derive(Default)]
pub struct Out {
    text: String,
    /// Formatting currently open (markers already written) in `text`.
    open: Fmt,
    /// Byte index in `text` just past the open markers.
    open_pos: usize,
    /// Emitted at the next line start, before any visible text.
    prefix: Option<String>,
}

impl Out {
    /// Queue a heading marker or list marker for the current paragraph. Replaces
    /// any prefix not yet emitted.
    pub fn set_line_prefix(&mut self, prefix: String) {
        self.prefix = Some(prefix);
    }

    pub fn has_line_prefix(&self) -> bool {
        self.prefix.is_some()
    }

    /// Drop a prefix queued but not yet emitted.
    pub fn clear_line_prefix(&mut self) {
        self.prefix = None;
    }

    /// Append visible text carrying `want` formatting.
    pub fn push_text(&mut self, s: &str, want: Fmt) {
        if s.is_empty() {
            return;
        }
        self.flush_prefix();
        if want == self.open {
            self.text.push_str(s);
            return;
        }
        // A span must not open on, or close around, whitespace — `** bold**` is
        // not emphasis to a markdown parser.
        let trimmed = s.trim_start_matches(char::is_whitespace);
        if trimmed.is_empty() {
            self.text.push_str(s);
            return;
        }
        let lead = &s[..s.len() - trimmed.len()];
        self.close_span();
        self.text.push_str(lead);
        self.open_span(want);
        self.text.push_str(trimmed);
    }

    pub fn push_char(&mut self, c: char, want: Fmt) {
        let mut buf = [0u8; 4];
        self.push_text(c.encode_utf8(&mut buf), want);
    }

    /// Append structural text (cell separators, markers) with no formatting.
    pub fn push_structural(&mut self, s: &str) {
        self.close_span();
        self.text.push_str(s);
    }

    /// Begin a markdown table row: `| ` at the start of a fresh line.
    ///
    /// A dedicated method rather than `set_line_prefix`, deliberately: the
    /// prefix slot is shared with headings and `\listtext`, and a `\pard`
    /// inside a cell can reach `clear_line_prefix`. A row opener must not be
    /// clobberable that way.
    pub fn open_row(&mut self) {
        self.close_span();
        self.prefix = None;
        if !self.ends_with_newline() {
            self.text.push('\n');
        }
        self.text.push_str("| ");
    }

    /// Write `s` as a line of its own.
    pub fn push_line(&mut self, s: &str) {
        self.close_span();
        if !self.ends_with_newline() {
            self.text.push('\n');
        }
        self.text.push_str(s);
        self.text.push('\n');
    }

    /// End the current line. Any prefix queued for an empty paragraph is dropped.
    pub fn break_line(&mut self) {
        self.close_span();
        self.prefix = None;
        self.text.push('\n');
    }

    pub fn ends_with_newline(&self) -> bool {
        self.text.is_empty() || self.text.ends_with('\n')
    }

    pub fn finish(mut self) -> String {
        self.close_span();
        normalize(&self.text)
    }

    fn flush_prefix(&mut self) {
        let Some(prefix) = self.prefix.take() else {
            return;
        };
        self.close_span();
        if !self.ends_with_newline() {
            self.text.push('\n');
        }
        self.text.push_str(&prefix);
    }

    fn open_span(&mut self, want: Fmt) {
        if want.bold {
            self.text.push_str("**");
        }
        if want.italic {
            self.text.push('*');
        }
        self.open = want;
        self.open_pos = self.text.len();
    }

    /// Close whatever is open, keeping the markers tight against the text.
    fn close_span(&mut self) {
        if self.open.is_none() {
            return;
        }
        let body = &self.text[self.open_pos..];
        if body.trim().is_empty() {
            // Nothing was emphasised — drop the opening markers again.
            let ws = body.to_string();
            self.text.truncate(self.open_pos - self.open.marker_len());
            self.text.push_str(&ws);
        } else {
            let keep = self.open_pos + body.trim_end_matches(char::is_whitespace).len();
            let tail = self.text[keep..].to_string();
            self.text.truncate(keep);
            if self.open.italic {
                self.text.push('*');
            }
            if self.open.bold {
                self.text.push_str("**");
            }
            self.text.push_str(&tail);
        }
        self.open = Fmt::NONE;
        self.open_pos = self.text.len();
    }
}

/// Collapse excess blank lines / trailing whitespace into clean markdown-ish text.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.split('\n') {
        let t = line.trim_end();
        if t.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(t.trim_start_matches(' '));
            out.push('\n');
        }
    }
    out.trim().to_string()
}
