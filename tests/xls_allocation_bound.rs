//! A malformed `.xls` must fail cleanly — never exhaust memory.
//!
//! Regression test for TECH_DEBT T12. With calamine 0.26, **9 of Apache POI's 15
//! `clusterfuzz-testcase-minimized-POIHSSFFuzzer-*.xls` fixtures** drove
//! allocation past 2 GB in ~0.3 s and the process was killed by the OS. The
//! worst input was **1,782 bytes producing >2 GB — roughly 1.2 million times
//! amplification**. Any service accepting uploaded spreadsheets could be killed
//! by one ~2 KB file.
//!
//! Crucially this was NOT something `catch_unwind` could cover (see R2): the
//! allocation happened inside `calamine::open_workbook_auto_from_rs` *before*
//! any panic, so there was nothing to catch. An OOM kill is strictly worse than
//! a panic — the caller cannot defend with `try`/`except`.
//!
//! Fixed by upgrading calamine 0.26 -> 0.35, which rejects these at format
//! detection before allocating. The three fixtures below are the smallest
//! previously-fatal inputs.
//!
//! **If this test ever OOMs or hangs the runner rather than failing, that IS the
//! regression** — the bound has been lost again. It is deliberately written to
//! surface that loudly rather than to tolerate it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chunks_rs::formats::xlsx;

/// The three smallest inputs that were fatal under calamine 0.26.
const FIXTURES: &[(&str, u64)] = &[
    ("poi_fuzz_6537773940867072.xls", 1782),
    ("poi_fuzz_4819588401201152.xls", 3182),
    ("poi_fuzz_6322470200934400.xls", 3347),
];

/// Generous: the fixed path rejects each of these in well under a millisecond.
/// This only exists to catch a pathological blow-up, not to police performance.
const BUDGET: Duration = Duration::from_secs(10);

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_files")
        .join("excel")
}

#[test]
fn hostile_xls_fails_cleanly_without_exhausting_memory() {
    let dir = corpus();
    if !dir.is_dir() {
        eprintln!("test_files corpus absent — skipping (see release.yml)");
        return;
    }

    let mut checked = 0usize;
    for (name, expected_size) in FIXTURES {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("missing fixture {name} — skipping");
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            size, *expected_size,
            "{name}: fixture changed size; the amplification figures in this \
             test's docs describe the original bytes"
        );

        let started = Instant::now();
        // `default` mode, ordinary arguments — exactly what get_chunks does.
        let result = xlsx::chunk(
            &path.to_string_lossy(),
            "default",
            1,
            1,
            0,
            true,
            Vec::new(),
            true,
            2000,
        );
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "{name}: a corrupt fuzzer fixture parsed successfully — if calamine \
             genuinely learned to read it, update this test deliberately"
        );
        assert!(
            elapsed < BUDGET,
            "{name}: took {elapsed:?} (budget {BUDGET:?}) — a blow-up like this \
             is how T12 presented before it was bounded"
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no fuzz fixtures were exercised — this test would pass vacuously"
    );
}
