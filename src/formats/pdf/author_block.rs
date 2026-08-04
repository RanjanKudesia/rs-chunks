//! Recover author/affiliation blocks that the PDF parser typed as tables.
//!
//! A paper's byline is laid out as columns — one author per column, with their
//! affiliation and email stacked beneath. liteparse's spatial heuristic sees a
//! grid and emits a markdown table, which reads *across* the columns and pairs
//! every author with the wrong affiliation:
//!
//! ```text
//! | Ashish Vaswani∗     | Noam Shazeer∗    |
//! |---|---|
//! | Google Brain        | Google Brain     |
//! | avaswani@google.com | noam@google.com  |
//! ```
//!
//! The column *is* the reading order, so such a table is transposed back:
//!
//! ```text
//! Ashish Vaswani∗, Google Brain, avaswani@google.com
//! Noam Shazeer∗, Google Brain, noam@google.com
//! ```
//!
//! The trigger is deliberately narrow — a small table with a row that is mostly
//! email addresses. A genuine data table is not rewritten, and a byline that
//! carries no email is left alone rather than guessed at.

/// Beyond this many rows a grid is a real table, not a byline.
const MAX_ROWS: usize = 5;

/// Rewrite any contact-block tables in `markdown`. Returns the input unchanged
/// when there are none, which is the overwhelmingly common case.
pub fn normalize(markdown: &str) -> String {
    if !markdown.contains('|') {
        return markdown.to_string();
    }
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    let mut rewrote = false;

    while i < lines.len() {
        match table_at(&lines, i) {
            Some(end) => {
                let table = &lines[i..end];
                match transpose_contact_block(table) {
                    Some(rows) => {
                        out.extend(rows);
                        rewrote = true;
                    }
                    None => out.extend(table.iter().map(|l| (*l).to_string())),
                }
                i = end;
            }
            None => {
                out.push(lines[i].to_string());
                i += 1;
            }
        }
    }
    if rewrote {
        out.join("\n")
    } else {
        markdown.to_string()
    }
}

/// End index of the markdown table starting at `start`, if one does.
fn table_at(lines: &[&str], start: usize) -> Option<usize> {
    if !is_row(lines.get(start)?) || !is_delimiter(lines.get(start + 1)?) {
        return None;
    }
    let mut end = start + 2;
    while end < lines.len() && is_row(lines[end]) {
        end += 1;
    }
    Some(end)
}

fn is_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_delimiter(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
}

fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// Transpose a table into one line per column, if it reads as a contact block.
fn transpose_contact_block(table: &[&str]) -> Option<Vec<String>> {
    let rows: Vec<Vec<String>> = table
        .iter()
        .filter(|l| !is_delimiter(l))
        .map(|l| split_row(l))
        .collect();
    if rows.len() < 2 || rows.len() > MAX_ROWS {
        return None;
    }
    let columns = rows.iter().map(Vec::len).max()?;
    if columns < 2 {
        return None;
    }
    // The signal: a row that is mostly email addresses. Real tables do not have
    // one, and a byline without one is not worth guessing at.
    let has_contact_row = rows.iter().any(|row| {
        let filled: Vec<&String> = row.iter().filter(|c| !c.is_empty()).collect();
        !filled.is_empty()
            && filled.iter().filter(|c| looks_like_email(c)).count() * 2 >= filled.len()
    });
    if !has_contact_row {
        return None;
    }

    let mut out = Vec::with_capacity(columns);
    for col in 0..columns {
        let parts: Vec<&str> = rows
            .iter()
            .filter_map(|row| row.get(col))
            .map(String::as_str)
            .filter(|c| !c.is_empty())
            .collect();
        if !parts.is_empty() {
            out.push(parts.join(", "));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A single token shaped like `local@host.tld`.
fn looks_like_email(cell: &str) -> bool {
    let s = cell.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((user, host)) = s.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && !host.contains('@')
        && host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transposes_a_byline() {
        let md = "# Title\n\n| A∗ | B∗ |\n|---|---|\n| Uni X | Uni Y |\n| a@x.edu | b@y.edu |\n\nAbstract";
        let got = normalize(md);
        assert!(got.contains("A∗, Uni X, a@x.edu"), "{got}");
        assert!(got.contains("B∗, Uni Y, b@y.edu"), "{got}");
        assert!(!got.contains('|'), "{got}");
    }

    #[test]
    fn leaves_a_real_table_alone() {
        let md = "| Model | BLEU |\n|---|---|\n| Transformer | 28.4 |\n| GNMT | 24.6 |";
        assert_eq!(normalize(md), md);
    }

    #[test]
    fn leaves_a_large_grid_alone_even_with_emails() {
        let mut md = String::from("| Name | Mail |\n|---|---|\n");
        for i in 0..8 {
            md.push_str(&format!("| P{i} | p{i}@x.com |\n"));
        }
        assert_eq!(normalize(md.trim_end()), md.trim_end());
    }

    #[test]
    fn leaves_a_byline_without_emails_alone() {
        let md = "| A | B |\n|---|---|\n| Uni X | Uni Y |";
        assert_eq!(normalize(md), md);
    }
}
