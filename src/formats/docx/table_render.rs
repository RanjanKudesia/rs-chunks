//! Table state accumulation and table -> Markdown pipe-table rendering.

/// Per-table accumulator used by [`parse_document_xml_blocks_streaming`].
/// A stack of these supports nested tables.
#[derive(Default)]
pub(super) struct TableState {
    pub(super) rows: Vec<Vec<String>>,
    pub(super) current_row: Vec<String>,
    pub(super) current_cell: String,
    pub(super) in_cell: bool,
    pub(super) in_tr_pr: bool,
    pub(super) in_header_row: bool,
    /// Column span of the current cell (`<w:gridSpan w:val="N"/>`). Defaults
    /// to 1 (no span). When > 1 the cell text is repeated into each spanned
    /// column position so the rendered markdown stays correctly aligned and
    /// every column retains its group-header context.
    pub(super) cell_span: usize,
    /// One flag per completed row indicating whether `<w:tblHeader/>` was
    /// present in its `<w:trPr>`.
    pub(super) header_row_flags: Vec<bool>,
    /// Cell content from the most recently completed row, indexed by column.
    /// Used to repeat content in vertically-merged continuation cells.
    pub(super) vmerge_col_content: Vec<String>,
    /// Whether the current cell is a vMerge continuation (no "restart").
    pub(super) cur_cell_is_vmerge_continuation: bool,
}

fn escape_md_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

pub(super) fn render_table_markdown(state: &TableState) -> String {
    if state.rows.is_empty() {
        return String::new();
    }
    let max_cols = state.rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    // Number of leading rows explicitly marked as header. Fallback: treat
    // first row as header when the table has more than one row.
    let mut header_count = state.header_row_flags.iter().take_while(|f| **f).count();
    if header_count == 0 && state.rows.len() > 1 {
        header_count = 1;
    }

    let mut out = String::new();
    for (i, row) in state.rows.iter().enumerate() {
        out.push('|');
        for col in 0..max_cols {
            let cell = row.get(col).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&escape_md_cell(cell));
            out.push_str(" |");
        }
        out.push('\n');
        if i + 1 == header_count && header_count < state.rows.len() {
            out.push('|');
            for _ in 0..max_cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

pub(super) fn render_table_inline(state: &TableState) -> String {
    let mut rows: Vec<String> = Vec::with_capacity(state.rows.len());
    for row in &state.rows {
        let cells: Vec<String> = row.iter().map(|c| c.replace(['\n', '\r'], " ")).collect();
        rows.push(cells.join(" | "));
    }
    rows.join("; ")
}
