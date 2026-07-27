


use super::common::{parse_html_blocks, remove_comments, HtmlBlockType};

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', " ")
}

fn blocks_to_markdown(blocks: &[super::common::HtmlBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for block in blocks {
        let md = match block.block_type {
            HtmlBlockType::Heading => {
                let level = (block.heading_level as usize).clamp(1, 6);
                let hashes = "#".repeat(level);
                format!("{} {}", hashes, block.content.trim())
            }
            HtmlBlockType::Paragraph => block.content.trim().to_string(),
            HtmlBlockType::Code => {
                format!("```\n{}\n```", block.content.trim_end())
            }
            HtmlBlockType::List => {
                let mut lines = Vec::new();
                for (i, item) in block.content.lines().enumerate() {
                    let t = item.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if block.is_ordered {
                        lines.push(format!("{}. {}", i + 1, t));
                    } else {
                        lines.push(format!("- {}", t));
                    }
                }
                if lines.is_empty() {
                    continue;
                }
                lines.join("\n")
            }
            HtmlBlockType::Table => {
                let rows: Vec<Vec<String>> = block
                    .content
                    .lines()
                    .map(|row| {
                        row.split(" | ")
                            .map(|cell| escape_pipe(cell.trim()))
                            .collect()
                    })
                    .filter(|row: &Vec<String>| !row.is_empty())
                    .collect();

                if rows.is_empty() {
                    continue;
                }
                let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);

                let mut out = String::new();
                for (i, row) in rows.iter().enumerate() {
                    out.push('|');
                    for col in 0..max_cols {
                        let cell = row.get(col).map(String::as_str).unwrap_or("");
                        out.push(' ');
                        out.push_str(cell);
                        out.push_str(" |");
                    }
                    out.push('\n');
                    if i == 0 && rows.len() > 1 {
                        out.push('|');
                        for _ in 0..max_cols {
                            out.push_str(" --- |");
                        }
                        out.push('\n');
                    }
                }
                out.trim_end().to_string()
            }
        };

        if !md.trim().is_empty() {
            parts.push(md);
        }
    }

    let mut result = parts.join("\n\n");
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

pub(crate) fn html_to_markdown_str(html: &str) -> String {
    let cleaned = remove_comments(html);
    let blocks = parse_html_blocks(&cleaned);
    blocks_to_markdown(&blocks)
}

