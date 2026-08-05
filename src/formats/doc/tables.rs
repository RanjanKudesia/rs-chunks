//! Table reconstruction for legacy `.doc`.
//!
//! A `.doc` has no table element. Cells are ordinary Word paragraphs whose
//! mark is a cell mark (`\x07`) instead of `\r`, flagged `sprmPFInTable`; the
//! paragraph that ends a row carries `sprmPFTtp` as well ([MS-DOC] 2.4.3). So
//! the structure is recovered by grouping the run of in-table paragraphs and
//! cutting it at those two marks — which is what makes per-row/per-column
//! metadata possible at all (TECH_DEBT #12).
//!
//! Cells can span several paragraphs (a `\r` inside a cell), so a cell is
//! everything accumulated up to its cell mark, not one paragraph.

/// One assembled table: rows of cell texts, in reading order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DocTable {
    pub rows: Vec<Vec<String>>,
}

/// The shape of a table, as reported in chunk metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableShape {
    pub rows: usize,
    pub columns: usize,
    pub cells: usize,
}

/// How a Word paragraph inside a table run was terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CellMark {
    /// `\r` — a line break *within* the current cell.
    Line,
    /// `\x07` without `sprmPFTtp` — the cell ends here.
    EndOfCell,
    /// `\x07` with `sprmPFTtp` — the row ends here.
    EndOfRow,
}

impl DocTable {
    /// Assemble a table from the paragraphs of one in-table run.
    pub fn from_units<'a>(units: impl IntoIterator<Item = (&'a str, CellMark)>) -> Self {
        let mut table = DocTable::default();
        let mut row: Vec<String> = Vec::new();
        let mut cell: Vec<&str> = Vec::new();

        let flush_cell = |cell: &mut Vec<&str>, row: &mut Vec<String>| {
            let text = cell.join(" ").trim().to_string();
            cell.clear();
            row.push(text);
        };

        for (text, mark) in units {
            let text = text.trim();
            if !text.is_empty() {
                cell.push(text);
            }
            match mark {
                CellMark::Line => {}
                CellMark::EndOfCell => flush_cell(&mut cell, &mut row),
                CellMark::EndOfRow => {
                    // A row mark's own paragraph carries no cell text, but a
                    // malformed file can leave text pending — keep it rather
                    // than drop content.
                    if !cell.is_empty() {
                        flush_cell(&mut cell, &mut row);
                    }
                    table.rows.push(std::mem::take(&mut row));
                }
            }
        }

        if !cell.is_empty() {
            flush_cell(&mut cell, &mut row);
        }
        if !row.is_empty() {
            table.rows.push(row);
        }
        table
    }

    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.iter().all(|c| c.is_empty()))
    }

    pub fn shape(&self) -> TableShape {
        TableShape {
            rows: self.rows.len(),
            columns: self.rows.iter().map(Vec::len).max().unwrap_or(0),
            cells: self.rows.iter().map(Vec::len).sum(),
        }
    }

    /// Render as a Markdown pipe table, matching the DOCX renderer: rows padded
    /// to the widest, pipes escaped, and **one** separator after the header row.
    pub fn to_markdown(&self) -> String {
        let max_cols = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if max_cols == 0 {
            return String::new();
        }
        // `.doc` records no header flag, so the DOCX fallback applies: the
        // first row heads a table that has more than one.
        let header_count = usize::from(self.rows.len() > 1);

        let mut out = String::new();
        for (i, row) in self.rows.iter().enumerate() {
            out.push('|');
            for col in 0..max_cols {
                out.push(' ');
                out.push_str(&escape_cell(row.get(col).map(String::as_str).unwrap_or("")));
                out.push_str(" |");
            }
            out.push('\n');
            if i + 1 == header_count && header_count < self.rows.len() {
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
}

fn escape_cell(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::CellMark::*;
    use super::*;

    #[test]
    fn cells_and_rows_are_cut_at_their_own_marks() {
        let table = DocTable::from_units(vec![
            ("Symbol", EndOfCell),
            ("Quantity", EndOfCell),
            ("Conversion", EndOfCell),
            ("", EndOfRow),
            ("B", EndOfCell),
            ("magnetic flux density", EndOfCell),
            ("1 G = 10 T", EndOfCell),
            ("", EndOfRow),
        ]);
        assert_eq!(
            table.rows,
            vec![
                vec!["Symbol", "Quantity", "Conversion"],
                vec!["B", "magnetic flux density", "1 G = 10 T"],
            ]
        );
        assert_eq!(
            table.shape(),
            TableShape {
                rows: 2,
                columns: 3,
                cells: 6
            }
        );
    }

    /// A `\r` inside a cell is a line break, not a cell boundary — treating it
    /// as one is how a three-column table turns into a ragged mess.
    #[test]
    fn a_multi_paragraph_cell_stays_one_cell() {
        let table = DocTable::from_units(vec![
            ("Conversion from Gaussian and", Line),
            ("CGS EMU to SI", EndOfCell),
            ("", EndOfRow),
        ]);
        assert_eq!(
            table.rows,
            vec![vec!["Conversion from Gaussian and CGS EMU to SI"]]
        );
    }

    #[test]
    fn a_row_missing_its_final_mark_is_still_kept() {
        let table = DocTable::from_units(vec![("a", EndOfCell), ("b", EndOfCell)]);
        assert_eq!(table.rows, vec![vec!["a", "b"]]);
    }

    #[test]
    fn short_rows_are_padded_and_only_the_header_gets_a_separator() {
        let table = DocTable {
            rows: vec![
                vec!["h1".into(), "h2".into(), "h3".into()],
                vec!["a".into()],
                vec!["b".into(), "c".into()],
            ],
        };
        assert_eq!(
            table.to_markdown(),
            "| h1 | h2 | h3 |\n| --- | --- | --- |\n| a |  |  |\n| b | c |  |"
        );
    }

    #[test]
    fn pipes_inside_a_cell_are_escaped() {
        let table = DocTable {
            rows: vec![vec!["a | b".into()]],
        };
        assert_eq!(table.to_markdown(), "| a \\| b |");
    }

    #[test]
    fn a_table_of_empty_cells_reports_itself_empty() {
        let table = DocTable::from_units(vec![("", EndOfCell), ("", EndOfRow)]);
        assert!(table.is_empty());
    }
}
