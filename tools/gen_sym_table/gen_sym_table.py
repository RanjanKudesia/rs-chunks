#!/usr/bin/env python3
"""Generate formats/docx/sym_table.rs from verified Unicode/Adobe sources.

Sources (SHA-256 pinned below, fetched 2026-08-27):
  dings.txt    unicode-org/unicodetools (normative per its header, Unicode V3 licence)
               format: UCS_HEX;INDEX[;INDEX]  where INDEX = F*1000 + decimal char code,
               F: 0=Webdings 1=Wingdings 2=Wingdings2 3=Wingdings3
  symbol.txt   unicode.org/Public/MAPPINGS/VENDORS/ADOBE  (unicode -> font code)
  zdingbat.txt same, for ZapfDingbats
  dingbats.csv mwilliamson/dingbat-to-unicode — USED ONLY AS A DIFFER, never as a source.

Policy: a target in any Private Use Area is dropped (a wrong glyph is worse than
a dropped one — the engine's standing rule). The generator FAILS if per-font
counts drift or if dings-vs-CSV value conflicts exceed the three adjudicated
rows (where N4384 + the UCD side with dings.txt).
"""
import csv, hashlib, sys
from pathlib import Path

HERE = Path(__file__).parent
SHA = {
    "dings.txt":    "85cea14b050d27ca8ed8afa59e9da96a43fb962b05d9d6f3218097b1d1858c43",
    "symbol.txt":   "deb78ca840a429311939b9d165890873f71fb23ef223ceeb144a6c6d641a7e52",
    "zdingbat.txt": "2d8128a7280cdd47d93272f13c580d7f31fb34586ff65eaf29c7457c224974c0",
}
for name, want in SHA.items():
    got = hashlib.sha256((HERE / name).read_bytes()).hexdigest()
    assert got == want, f"{name}: sha mismatch {got}"

def is_pua(cp): return 0xE000 <= cp <= 0xF8FF or 0xF0000 <= cp <= 0x10FFFD

FONTS = {0: "Webdings", 1: "Wingdings", 2: "Wingdings 2", 3: "Wingdings 3"}
tables = {n: {} for n in FONTS.values()}
for line in (HERE / "dings.txt").read_text().splitlines():
    line = line.split("#")[0].strip()
    if not line: continue
    parts = line.split(";")
    cp = int(parts[0], 16)
    for idx in parts[1:]:
        idx = int(idx)
        font, code = FONTS[idx // 1000], idx % 1000
        assert 32 <= code <= 255, (font, code)
        if is_pua(cp): continue
        # first mapping wins on duplicate codes (dings lists UCS-major)
        tables[font].setdefault(code, cp)

def adobe(fname, fontname):
    t = {}
    for line in (HERE / fname).read_text().splitlines():
        if line.startswith("#") or not line.strip(): continue
        cols = line.split("\t")
        cp, code = int(cols[0], 16), int(cols[1], 16)
        if is_pua(cp): continue
        # reverse map: prefer the lowest codepoint (SPACE over NBSP for 0x20)
        if code not in t or cp < t[code]:
            t[code] = cp
    tables[fontname] = t

adobe("symbol.txt", "Symbol")
# Adobe many-to-one adjudications. Code 0x6D carries both U+00B5 MICRO SIGN and
# U+03BC GREEK SMALL LETTER MU in symbol.txt; the font position is the Greek
# alphabet's mu (0x61-0x7A IS the Greek alphabet), so the letter wins over the
# compatibility character.
tables["Symbol"][0x6D] = 0x03BC
adobe("zdingbat.txt", "ZapfDingbats")

counts = {n: len(t) for n, t in tables.items()}
print("counts:", counts)
# dings.txt raw refs: 222/221/216/207 minus any PUA-target rows
assert counts["Webdings"] >= 215 and counts["Wingdings"] >= 215, counts
assert counts["Wingdings 2"] >= 205 and counts["Wingdings 3"] >= 195, counts
assert counts["Symbol"] >= 150 and counts["ZapfDingbats"] >= 150, counts

# ---- differ against dingbats.csv ----
ADJUDICATED = {("Wingdings", 54), ("Wingdings", 72), ("Wingdings", 119)}
csv_map = {}
with open(HERE / "dingbats.csv") as f:
    for row in csv.DictReader(f):
        name = {"Wingdings 2": "Wingdings 2", "Wingdings 3": "Wingdings 3"}.get(
            row["Typeface name"], row["Typeface name"])
        csv_map[(name, int(row["Dingbat dec"]))] = int(row["Unicode dec"])
# The CSV is a DIFFER, not a source. A conflict where the vendor file itself is
# unambiguous (exactly one Adobe row for that code) is a CSV defect — recorded,
# not fatal. Verified 2026-08-27: codes 0x27/0x2A/0x7E/0xE1/0xF1 have a single
# Adobe row each and the CSV chose ASCII/CJK lookalikes. A conflict where OUR
# side is ambiguous and unadjudicated is fatal.
adobe_rows = {}
for line in (HERE / "symbol.txt").read_text().splitlines():
    if line.startswith("#") or not line.strip(): continue
    c = line.split("\t")
    adobe_rows.setdefault(int(c[1], 16), []).append(int(c[0], 16))
SYMBOL_MANY_TO_ONE_ADJUDICATED = {0x6D}
csv_defects, fatal = [], []
for (font, code), csv_cp in csv_map.items():
    ours = tables.get(font, {}).get(code)
    if ours is None or ours == csv_cp or (font, code) in ADJUDICATED:
        continue
    if font == "Symbol":
        if len(adobe_rows.get(code, [])) == 1 or code in SYMBOL_MANY_TO_ONE_ADJUDICATED:
            csv_defects.append((font, code, hex(ours), hex(csv_cp)))
            continue
    fatal.append((font, code, hex(ours), hex(csv_cp)))
print(f"csv rows={len(csv_map)}  csv defects (vendor unambiguous)={len(csv_defects)}  fatal={len(fatal)}")
for c in fatal[:10]: print("  FATAL", c)
assert not fatal, "unexplained divergence from the differ — investigate before emitting"

# ---- emit rust ----
out = []
out.append("//! Font-glyph to Unicode table for `<w:sym>`. GENERATED — do not edit.")
out.append("//!")
out.append("//! Generated by `gen_sym_table.py` from Unicode's `dings.txt`")
out.append("//! (unicode-org/unicodetools, normative per its header; successor data of")
out.append("//! WG2 N4384) and Adobe's `symbol.txt`/`zdingbat.txt` vendor mappings.")
out.append("//! Sources are SHA-256-pinned in the generator; a Private-Use-Area target")
out.append("//! is dropped rather than guessed. See TECH_DEBT F9.")
out.append("")
for font in ["Symbol", "Webdings", "Wingdings", "Wingdings 2", "Wingdings 3", "ZapfDingbats"]:
    ident = font.upper().replace(" ", "_")
    rows = sorted(tables[font].items())
    out.append(f"pub(super) static {ident}: &[(u8, char)] = &[")
    for code, cp in rows:
        out.append(f"    (0x{code:02X}, '\\u{{{cp:04X}}}'),")
    out.append("];")
    out.append("")
out.append("/// Look up a `<w:sym>` character. `code` is the low byte (the spec stores")
out.append("/// `F0xx`; ISO/IEC 29500-1 says the character code is the low octet, so the")
out.append("/// caller masks). Returns `None` for an unmapped glyph — emit nothing:")
out.append("/// a wrong character is worse than a dropped one.")
out.append("pub(super) fn sym_lookup(font: &str, code: u8) -> Option<char> {")
out.append("    let table = match font.trim() {")
out.append('        f if f.eq_ignore_ascii_case("Symbol") => SYMBOL,')
out.append('        f if f.eq_ignore_ascii_case("Webdings") => WEBDINGS,')
out.append('        f if f.eq_ignore_ascii_case("Wingdings") => WINGDINGS,')
out.append('        f if f.eq_ignore_ascii_case("Wingdings 2") => WINGDINGS_2,')
out.append('        f if f.eq_ignore_ascii_case("Wingdings 3") => WINGDINGS_3,')
out.append('        f if f.eq_ignore_ascii_case("ZapfDingbats")')
out.append('            || f.eq_ignore_ascii_case("Zapf Dingbats") => ZAPFDINGBATS,')
out.append("        _ => return None,")
out.append("    };")
out.append("    table.binary_search_by_key(&code, |&(c, _)| c).ok().map(|i| table[i].1)")
out.append("}")
(HERE / "sym_table.rs").write_text("\n".join(out) + "\n")
print(f"emitted sym_table.rs: {len((HERE/'sym_table.rs').read_text().splitlines())} lines")
# sanity: the fixtures' expected glyphs
assert tables["Wingdings"][0xFC & 0xFF] == 0x2713, hex(tables["Wingdings"].get(0xFC, 0))
assert tables["Wingdings"][0x4A] == 0x263A
print("fixture checks: F0FC->U+2713 CHECK MARK, F04A->U+263A SMILING FACE  OK")
