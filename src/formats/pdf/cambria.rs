//! Cambria's glyph order, for subsets that name glyphs by bare index.
//!
//! `pdfjs_TAMReview.pdf` sets its 24-page body in `EMMOLK+Cambria`, a CFF subset
//! whose `/Encoding /Differences` names every glyph `/g18`, `/g152`, `/g135` …
//! — indices into the *retail* font — and which carries no `/ToUnicode`. Nothing
//! in the file states what those glyphs are, so 49,909 glyphs decoded to
//! nothing and only the cover page and back matter (set in Times) came out.
//!
//! ## Where this table comes from
//!
//! Not from a copy of Cambria — there isn't one, and [#84](TECH_DEBT.md) was
//! wrong to say one was needed. The font's own `/Widths` array is a fingerprint,
//! and the *same document* carries the witness that reads it:
//! `EMMONL+Cambria-Bold` and `EMMPCK+Cambria-Italic` are subsets of the same
//! family that name their glyphs properly (`/A`, `/comma`) **and** ship a
//! `/ToUnicode`. Their widths give the family's relative metric structure, and
//! the regular face reproduces it exactly on three contiguous GID runs:
//!
//! | | bold (known letters) | regular (GIDs 4–29 / 131–156) |
//! |---|---|---|
//! | widest capital | `M` 846 | GID 16 = 815 |
//! | narrowest capital | `I` 350 < `J` 341 | GID 12 = 324, GID 13 = 307 |
//! | `F` and `L` share a width | 551 = 551 | GID 9 = 537 = GID 15 |
//! | `b` and `q` share a width | 591 = 591 | GID 132 = 547 = GID 147 |
//! | `d`≈`h`≈`p`≈`u` | 597 | 555 / 552 / 556 / 552 |
//! | narrow trio `i` > `l` > `j` | 314 > 308 > 302 | 278 > 271 > 266 |
//! | widest lowercase | `m` 890 | GID 143 = 832 |
//!
//! and GIDs 882–891 all share one width (554), which is what a font does with
//! tabular figures. Decoding the document under that reading returns clean
//! English — *"With growing technology needs in the 1970's"*, *"(Davis, 1985,
//! p. 10)"*, *"Fishbein and Ajzen (1975)"* — which is the actual proof; the
//! width structure is only what made it worth trying.
//!
//! The punctuation entries were each read back from the decoded text: GID 486 is
//! a hyphen because it appears in *"self-efficacy"*, *"meta-analysis"* and
//! *"job-related"*; GID 428 is an ampersand because it appears in *"Lee, Kozar,
//! & Larsen, 2003"*. GIDs 820 and 821 are **deliberately absent** — 107 glyph
//! instances inside one rating-scale figure that context does not pin down, and
//! a guess there would be exactly the failure this module is built to avoid.
//!
//! ## Why it refuses rather than guesses
//!
//! A GID table is a property of *one build* of a font. Against a version whose
//! glyph order had shifted, this table would emit plausible **wrong** letters,
//! which is worse than the silence it replaces — a reader cannot tell corrupt
//! prose from a bad scan. So every mapping is checked against the width the
//! font itself declares, and a single disagreement rejects the whole font and
//! restores the old behaviour. See [`resolve`].

use std::collections::HashMap;

/// GID → (character, the width retail Cambria declares for it, in 1/1000 em).
///
/// Generated from `pdfjs_TAMReview.pdf`'s own `/Widths` rather than typed out,
/// because 79 hand-copied numbers is how a wrong one gets shipped.
///
/// `None` means the document never draws that glyph, so there is no reference
/// width to check: only `Z`, which is the 26th slot of a run whose other 25 are
/// `A`–`Y` and whose lowercase twin (131–156) is a complete `a`–`z`.
///
/// Sorted by GID — [`lookup`] binary-searches it.
const CAMBRIA: &[(u16, char, Option<u16>)] = &[
    (3, ' ', Some(220)),
    (4, 'A', Some(623)),
    (5, 'B', Some(611)),
    (6, 'C', Some(563)),
    (7, 'D', Some(662)),
    (8, 'E', Some(575)),
    (9, 'F', Some(537)),
    (10, 'G', Some(611)),
    (11, 'H', Some(687)),
    (12, 'I', Some(324)),
    (13, 'J', Some(307)),
    (14, 'K', Some(629)),
    (15, 'L', Some(537)),
    (16, 'M', Some(815)),
    (17, 'N', Some(681)),
    (18, 'O', Some(653)),
    (19, 'P', Some(568)),
    (20, 'Q', Some(653)),
    (21, 'R', Some(621)),
    (22, 'S', Some(496)),
    (23, 'T', Some(593)),
    (24, 'U', Some(648)),
    (25, 'V', Some(604)),
    (26, 'W', Some(921)),
    (27, 'X', Some(571)),
    (28, 'Y', Some(570)),
    (29, 'Z', None),
    (131, 'a', Some(488)),
    (132, 'b', Some(547)),
    (133, 'c', Some(441)),
    (134, 'd', Some(555)),
    (135, 'e', Some(488)),
    (136, 'f', Some(303)),
    (137, 'g', Some(494)),
    (138, 'h', Some(552)),
    (139, 'i', Some(278)),
    (140, 'j', Some(266)),
    (141, 'k', Some(524)),
    (142, 'l', Some(271)),
    (143, 'm', Some(832)),
    (144, 'n', Some(558)),
    (145, 'o', Some(531)),
    (146, 'p', Some(556)),
    (147, 'q', Some(547)),
    (148, 'r', Some(414)),
    (149, 's', Some(430)),
    (150, 't', Some(338)),
    (151, 'u', Some(552)),
    (152, 'v', Some(504)),
    (153, 'w', Some(774)),
    (154, 'x', Some(483)),
    (155, 'y', Some(504)),
    (156, 'z', Some(455)),
    (428, '&', Some(688)),  // "Lee, Kozar, & Larsen, 2003"
    (481, ',', Some(205)),
    (482, ';', Some(264)),  // "its propositions and possible limitations; 2)"
    (483, ':', Some(264)),  // "Figure 1: Conceptual model for technology acceptance"
    (484, '.', Some(205)),
    (486, '-', Some(332)),  // "self-efficacy", "meta-analysis", "job-related"
    (491, '?', Some(422)),  // "Why Do People Use Information Technology?"
    (495, '\u{2019}', Some(221)), // "the 1970's", "one's intention to perform"
    (498, '\u{201C}', Some(375)), // labelled "neutral", as shown below
    (499, '\u{201D}', Some(375)),
    (512, '/', Some(490)),  // "she/he", "adopting/using", "5/6"
    (514, '\u{2013}', Some(500)), // en dash, in the reference list's page ranges
    (523, '(', Some(382)),
    (524, ')', Some(382)),
    (882, '0', Some(554)),
    (883, '1', Some(554)),
    (884, '2', Some(554)),
    (885, '3', Some(554)),
    (886, '4', Some(554)),
    (887, '5', Some(554)),
    (888, '6', Some(554)),
    (889, '7', Some(554)),
    (890, '8', Some(554)),
    (891, '9', Some(554)),
    (938, '+', Some(554)),  // the Theory of Reasoned Action's identity, BI = A + SN
    (945, '=', Some(554)),
];

/// How far a declared width may sit from the reference and still be the same
/// font. Widths are integers in 1/1000 em and a matching build reproduces them
/// exactly; the slack is only for a producer that re-rounds them.
const WIDTH_TOLERANCE: i32 = 2;

/// Below this many *checkable* glyphs the width test has not proven anything,
/// so the font is refused. This is what keeps the table off a face it does not
/// describe: TAMReview's own Cambria-Bold and Cambria-Italic subsets carry two
/// bare-index names each, and their metrics are a different face's.
const MIN_SAMPLE: usize = 8;

/// Map a Cambria subset's bare-index glyph names to characters, or refuse.
///
/// `bare` is the `(code, gid)` pairs `/Differences` gave as `/gNNN`, `declared`
/// the font's own `/Widths` in em units. Returns `None` — meaning "keep
/// dropping these glyphs" — unless the font is the regular face *and* every
/// width it declares for a glyph in the table agrees with the table.
///
/// All-or-nothing is the point. Half a font decoded under a skewed table is
/// prose with wrong letters scattered through it, and nothing downstream can
/// detect that; dropping the font is at least honest.
pub(super) fn resolve(
    base_font: &str,
    bare: &[(usize, u16)],
    declared: &HashMap<u32, f32>,
) -> Option<Vec<(usize, char)>> {
    if !is_regular_face(base_font) || bare.is_empty() {
        return None;
    }
    let mut resolved = Vec::with_capacity(bare.len());
    let mut checked = 0usize;
    for &(code, gid) in bare {
        let Some((ch, reference)) = lookup(gid) else { continue };
        if let (Some(reference), Some(width)) = (reference, declared.get(&(code as u32))) {
            if (thousandths(*width) - i32::from(reference)).abs() > WIDTH_TOLERANCE {
                return None;
            }
            checked += 1;
        }
        resolved.push((code, ch));
    }
    (checked >= MIN_SAMPLE).then_some(resolved)
}

/// The table describes retail Cambria **regular**. The bold and italic faces
/// have their own metrics and their own glyph order, and this corpus carries no
/// evidence for either, so they are left alone rather than mapped on a guess.
fn is_regular_face(base_font: &str) -> bool {
    // `EMMOLK+Cambria` — a subset tag is six uppercase letters and a `+`.
    let name = base_font.split_once('+').map_or(base_font, |(_, rest)| rest);
    name.eq_ignore_ascii_case("Cambria")
}

fn lookup(gid: u16) -> Option<(char, Option<u16>)> {
    CAMBRIA
        .binary_search_by(|(g, _, _)| g.cmp(&gid))
        .ok()
        .map(|i| (CAMBRIA[i].1, CAMBRIA[i].2))
}

/// `/Widths` is parsed into em units; the table is in the 1/1000 em the file
/// actually states, which is where an exact comparison is meaningful.
fn thousandths(em: f32) -> i32 {
    (em * 1000.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GID runs are contiguous and complete — the property the whole
    /// reading rests on. A gap would mean the alphabet is not laid out the way
    /// the width fingerprint says it is.
    #[test]
    fn the_table_is_sorted_and_its_alphabet_runs_are_complete() {
        assert!(CAMBRIA.windows(2).all(|w| w[0].0 < w[1].0), "table must stay sorted for lookup");
        for (base, first, last) in [(4u16, 'A', 'Z'), (131, 'a', 'z'), (882, '0', '9')] {
            let span = u16::from(u8::try_from(last as u32 - first as u32).unwrap());
            for offset in 0..=span {
                let want = char::from_u32(first as u32 + u32::from(offset)).unwrap();
                assert_eq!(lookup(base + offset).map(|(c, _)| c), Some(want));
            }
        }
    }

    fn tam_widths() -> HashMap<u32, f32> {
        // Codes 0.. carrying the GIDs below, at the widths TAMReview declares.
        CAMBRIA
            .iter()
            .enumerate()
            .filter_map(|(i, (_, _, w))| w.map(|w| (i as u32, f32::from(w) / 1000.0)))
            .collect()
    }

    fn tam_bare() -> Vec<(usize, u16)> {
        CAMBRIA.iter().enumerate().map(|(i, (gid, _, _))| (i, *gid)).collect()
    }

    #[test]
    fn a_matching_font_resolves_every_glyph_in_the_table() {
        let bare = tam_bare();
        let resolved = resolve("EMMOLK+Cambria", &bare, &tam_widths()).expect("should resolve");
        assert_eq!(resolved.len(), CAMBRIA.len());
        // Every code got the character its GID names, not a shifted one.
        for (code, gid) in &bare {
            let want = lookup(*gid).map(|(c, _)| c);
            assert_eq!(resolved.iter().find(|(c, _)| c == code).map(|(_, c)| *c), want);
        }
    }

    /// The gate is the whole safety argument, so it is tested by breaking it —
    /// one glyph 3/1000 em off, which is what a different build of the font
    /// would look like, must take the *entire* font back to silence.
    #[test]
    fn one_disagreeing_width_refuses_the_whole_font() {
        let mut widths = tam_widths();
        let a = tam_bare().iter().position(|(_, gid)| *gid == 4).unwrap() as u32;
        widths.insert(a, (623.0 + 3.0) / 1000.0);
        assert_eq!(resolve("EMMOLK+Cambria", &tam_bare(), &widths), None);

        // …and a rounding-sized difference is not treated as skew.
        widths.insert(a, (623.0 + 2.0) / 1000.0);
        assert!(resolve("EMMOLK+Cambria", &tam_bare(), &widths).is_some());
    }

    #[test]
    fn another_face_and_too_small_a_sample_are_both_refused() {
        assert_eq!(resolve("EMMONL+Cambria-Bold", &tam_bare(), &tam_widths()), None);
        assert_eq!(resolve("EMMPCK+Cambria-Italic", &tam_bare(), &tam_widths()), None);
        assert_eq!(resolve("ABCDEF+Calibri", &tam_bare(), &tam_widths()), None);
        // Cambria-Bold's actual shape: two bare names, nothing to check against.
        let two = vec![(2usize, 3u16), (3, 486)];
        assert_eq!(resolve("EMMOLK+Cambria", &two, &tam_widths()), None);
    }

    /// GIDs 820 and 821 are used by the document and deliberately unmapped.
    #[test]
    fn the_glyphs_context_could_not_identify_stay_unmapped() {
        assert_eq!(lookup(820), None);
        assert_eq!(lookup(821), None);
    }
}
