#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleKind {
    Normal,
    Heading(u8),
    ListParagraph,
    Unknown,
}

pub struct StyleSheet {
    pub styles: Vec<StyleKind>,
}

impl StyleSheet {
    pub fn kind(&self, istd: usize) -> StyleKind {
        self.styles.get(istd).cloned().unwrap_or(StyleKind::Unknown)
    }
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn decode_utf16(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        units.push(u16::from_le_bytes([bytes[i], bytes[i + 1]]));
        i += 2;
    }
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

fn heading_level_from_name(name_lc: &str) -> Option<u8> {
    for prefix in ["heading", "titre", "überschrift"] {
        if let Some(rest) = name_lc.strip_prefix(prefix) {
            let digits: String = rest.chars().skip_while(|c| !c.is_ascii_digit()).collect();
            if let Some(ch) = digits.chars().next() {
                if let Some(n) = ch.to_digit(10) {
                    if (1..=9).contains(&n) {
                        return Some(n as u8);
                    }
                }
            }
            return Some(1);
        }
    }
    None
}

fn classify_style_name(name: &str) -> StyleKind {
    let lower = name.trim().to_ascii_lowercase();
    if let Some(level) = heading_level_from_name(&lower) {
        return StyleKind::Heading(level);
    }
    if lower == "normal" || lower == "default paragraph font" {
        return StyleKind::Normal;
    }
    if lower.starts_with("list paragraph")
        || lower.starts_with("list bullet")
        || lower.starts_with("list number")
    {
        return StyleKind::ListParagraph;
    }
    StyleKind::Unknown
}

pub fn parse_stylesheet(
    table_stream: &[u8],
    fc_stshf: u32,
    lcb_stshf: u32,
) -> Result<StyleSheet, String> {
    let start = fc_stshf as usize;
    let len = lcb_stshf as usize;
    let end = start
        .checked_add(len)
        .ok_or_else(|| "Stylesheet offset overflow".to_string())?;
    let data = table_stream
        .get(start..end)
        .ok_or_else(|| "Stylesheet points outside table stream".to_string())?;

    if data.len() < 14 {
        return Ok(StyleSheet { styles: Vec::new() });
    }

    // [MS-DOC] 2.9.271 STSH = LPStshi + rglpstd, and 2.9.272 STSHI starts
    // cstd(2) cbSTDBaseInFile(2). So the layout is:
    //   0        cbStshi        — size of the STSHI that follows
    //   2        cstd           — number of style definitions
    //   4        cbSTDBaseInFile
    //   2+cbStshi                rglpstd begins
    //
    // Reading cstd at 0 and cbSTDBaseInFile at 2 took cbStshi as the style
    // count and the style count as the STD header size, then started the walk
    // at a hardcoded 14. On sample.doc that is (20, 39, 14) against a true
    // (39, 10, 22): every style name decoded from the wrong offset, so no style
    // was ever recognised as a heading and `.doc` fell back to guessing
    // headings from capitalisation — which is why every one reported level 2.
    let cb_stshi = read_u16(data, 0).unwrap_or(0) as usize;
    let cstd = read_u16(data, 2).unwrap_or(0) as usize;
    let cb_std_base_in_file = read_u16(data, 4).unwrap_or(0) as usize;

    let mut pos = 2usize.saturating_add(cb_stshi);
    let mut styles = Vec::with_capacity(cstd.min(4096));

    for _ in 0..cstd {
        let cb_std = match read_u16(data, pos) {
            Some(v) => v as usize,
            None => break,
        };
        pos += 2;

        if cb_std == 0 {
            styles.push(StyleKind::Unknown);
            continue;
        }

        let std_end = match pos.checked_add(cb_std) {
            Some(v) => v,
            None => {
                styles.push(StyleKind::Unknown);
                break;
            }
        };
        let std = match data.get(pos..std_end) {
            Some(v) => v,
            None => {
                styles.push(StyleKind::Unknown);
                break;
            }
        };

        let mut kind = StyleKind::Unknown;
        if cb_std_base_in_file + 2 <= std.len() {
            let cch_name = read_u16(std, cb_std_base_in_file).unwrap_or(0) as usize;
            let name_bytes_start = cb_std_base_in_file + 2;
            let name_bytes_len = cch_name.saturating_mul(2);
            let name_bytes_end = name_bytes_start.saturating_add(name_bytes_len);
            if let Some(name_bytes) = std.get(name_bytes_start..name_bytes_end) {
                let name = decode_utf16(name_bytes);
                kind = classify_style_name(&name);
            }
        }

        styles.push(kind);
        pos = std_end;
    }

    while styles.len() < cstd {
        styles.push(StyleKind::Unknown);
    }

    Ok(StyleSheet { styles })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an STSH: cbStshi, an STSHI of that size, then one LPStd per name.
    fn stsh(names: &[&str]) -> Vec<u8> {
        const CB_STSHI: usize = 18;
        const CB_STD_BASE: usize = 10;
        let mut out = Vec::new();
        out.extend_from_slice(&(CB_STSHI as u16).to_le_bytes());
        out.extend_from_slice(&(names.len() as u16).to_le_bytes());
        out.extend_from_slice(&(CB_STD_BASE as u16).to_le_bytes());
        out.resize(2 + CB_STSHI, 0);

        for name in names {
            let units: Vec<u16> = name.encode_utf16().collect();
            let mut std = vec![0u8; CB_STD_BASE];
            std.extend_from_slice(&(units.len() as u16).to_le_bytes());
            for u in &units {
                std.extend_from_slice(&u.to_le_bytes());
            }
            std.extend_from_slice(&0u16.to_le_bytes()); // terminating NUL
            out.extend_from_slice(&(std.len() as u16).to_le_bytes());
            out.extend_from_slice(&std);
        }
        out
    }

    /// The shipped reader took cbStshi for cstd, cstd for cbSTDBaseInFile, and
    /// started at a hardcoded 14 — so no name ever decoded and every style
    /// classified as `Unknown`.
    #[test]
    fn style_names_decode_at_their_real_offsets() {
        let data = stsh(&["Normal", "Heading 1", "Heading 2", "List Paragraph", "Body"]);
        let sheet = parse_stylesheet(&data, 0, data.len() as u32).unwrap();
        assert_eq!(
            sheet.styles,
            vec![
                StyleKind::Normal,
                StyleKind::Heading(1),
                StyleKind::Heading(2),
                StyleKind::ListParagraph,
                StyleKind::Unknown,
            ]
        );
    }

    /// `istd` indexes this vector directly, so an empty slot must still occupy
    /// its position or every later style shifts.
    #[test]
    fn istd_indexes_the_style_vector_directly() {
        let data = stsh(&["Normal", "Heading 1", "Heading 2", "Heading 3"]);
        let sheet = parse_stylesheet(&data, 0, data.len() as u32).unwrap();
        assert_eq!(sheet.kind(3), StyleKind::Heading(3));
        assert_eq!(sheet.kind(99), StyleKind::Unknown);
    }

    #[test]
    fn a_truncated_stylesheet_degrades_instead_of_panicking() {
        let mut data = stsh(&["Normal", "Heading 1"]);
        data.truncate(2 + 18 + 3);
        let sheet = parse_stylesheet(&data, 0, data.len() as u32).unwrap();
        assert_eq!(sheet.styles.len(), 2, "cstd is still honoured");
    }
}
