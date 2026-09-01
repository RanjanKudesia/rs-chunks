//! `<w:sym>` glyphs and run-content tabs must reach the output.
//!
//! Both were silently dropped. A Wingdings check mark or smiley vanished
//! entirely (F9's last open half), and a `<w:tab/>` produced no character at
//! all — 283 tabs in one fixture became zero characters, fusing words and
//! flattening tab-delimited tables into prose (the `.dotm` reviewer's
//! priority correction to R2).
//!
//! The sym table is GENERATED from Unicode's `dings.txt` and Adobe's vendor
//! mappings (`tools/gen_sym_table/`), never hand-typed: a wrong glyph is worse
//! than a dropped one, so an unmapped code still emits nothing.

use chunks_rs::formats::docx;

fn fixture(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_files")
        .join(rel);
    assert!(p.is_file(), "required fixture missing: {}", p.display());
    p.to_str().unwrap().to_string()
}

#[test]
fn a_wingdings_sym_becomes_its_unicode_character() {
    // sym.docx's body is `<w:sym w:font="Wingdings" w:char="F0FC"/>` — the
    // Wingdings check mark, U+2713 per dings.txt.
    let md = docx::to_markdown(&fixture("docx_synthetic/sym.docx")).expect("must parse");
    assert!(
        md.contains('\u{2713}'),
        "the w:sym glyph did not surface: {md:?}"
    );
}

#[test]
fn run_tabs_separate_words_mid_line() {
    // tika_02_testDOTM.dotm carries 171 run-content tabs formatting an
    // 88-row tab-delimited table. Before the fix its output contained zero
    // tab characters and adjacent fields fused.
    let md = docx::to_markdown(&fixture("dotm/tika_02_testDOTM.dotm")).expect("must parse");
    let tabs = md.matches('\t').count();
    assert!(
        tabs > 50,
        "run tabs still vanish (found {tabs} in output)"
    );
}

#[test]
fn no_output_line_starts_with_a_tab() {
    // The GFM hazard the rendering rule exists for: a tab in a line's leading
    // whitespace turns the line into indented code. Leading tabs are dropped.
    for f in ["dotm/tika_02_testDOTM.dotm", "docx/poi_bug65649.docx"] {
        let md = docx::to_markdown(&fixture(f)).expect("must parse");
        for (i, line) in md.lines().enumerate() {
            assert!(
                !line.starts_with('\t'),
                "{f}: line {i} starts with a tab — indented-code hazard"
            );
        }
    }
}

#[test]
fn a_tab_stop_definition_emits_nothing() {
    // `w:pPr/w:tabs/w:tab w:val=".." w:pos=".."` is layout, not text. The
    // guard is the attribute check; this pins that a plain document whose
    // paragraphs carry tab-stop definitions gains no stray tabs.
    // poi_drawing.docx has pPr tab stops and no run tabs at paragraph starts.
    let md = docx::to_markdown(&fixture("docx/poi_drawing.docx")).expect("must parse");
    for line in md.lines() {
        assert!(!line.starts_with('\t'), "stray leading tab: {line:?}");
    }
}
