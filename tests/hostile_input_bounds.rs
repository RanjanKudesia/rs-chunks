//! Small crafted files must not kill the process.
//!
//! A stack overflow and an out-of-memory abort are **not panics** — they are
//! process aborts, so `catch_unwind` cannot intercept them and no caller, in
//! any language, can defend against them. TECH_DEBT T12 was one of these, fixed
//! by upgrading a dependency; these are the same shape in our own code.
//!
//! Measured before the fix: a **480 KB** file of nested OfficeArt container
//! headers produced `fatal runtime error: stack overflow, aborting`
//! (SIGABRT). Each level costs only the 8-byte record header.
//!
//! Every case here runs on a thread with a deliberately **small stack**. That
//! makes the test strictly harder than production and, more importantly, means
//! an unbounded recursion shows up as a failure on a thread rather than taking
//! the whole test binary down with it.

use std::time::{Duration, Instant};

/// Small enough that ~2k frames of these walkers would exhaust it.
const SMALL_STACK: usize = 512 * 1024;
const BUDGET: Duration = Duration::from_secs(20);

fn on_small_stack<F: FnOnce() + Send + 'static>(name: &'static str, f: F) {
    let started = Instant::now();
    let handle = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .name(name.to_string())
        .spawn(f)
        .expect("spawn");
    handle
        .join()
        .unwrap_or_else(|_| panic!("{name}: worker died — unbounded recursion or allocation"));
    assert!(
        started.elapsed() < BUDGET,
        "{name}: took {:?}, over budget — a bound is missing",
        started.elapsed()
    );
}

/// `levels` nested OfficeArt containers: 8 bytes each, every one declaring that
/// the rest of the buffer is its body.
fn nested_odraw(levels: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(levels * 8);
    for i in 0..levels {
        let remaining = ((levels - i - 1) * 8) as u32;
        v.extend_from_slice(&0x000Fu16.to_le_bytes()); // recVer = 0xF (container)
        v.extend_from_slice(&0xF000u16.to_le_bytes()); // OfficeArtDggContainer
        v.extend_from_slice(&remaining.to_le_bytes());
    }
    v
}

#[test]
fn deeply_nested_odraw_containers_do_not_exhaust_the_stack() {
    // 200k levels = 1.6 MB. Pre-fix this aborted at ~60k on a full-size stack.
    let data = nested_odraw(200_000);
    on_small_stack("odraw-doc", move || {
        // Reached through the .doc image extractor, which is what a real file
        // drives. A missing bound shows up as a dead thread.
        let _ = chunks_rs::get_chunks_from_bytes(&data, "probe.doc", "default", 3, 1, 3, 15);
    });
}

#[test]
fn deeply_nested_odraw_containers_are_safe_for_ppt() {
    let data = nested_odraw(200_000);
    on_small_stack("odraw-ppt", move || {
        let _ = chunks_rs::get_chunks_from_bytes(&data, "probe.ppt", "default", 3, 1, 3, 15);
    });
}

/// RTF group nesting is iterative (a `Vec<GroupState>`), so this is a memory
/// bound rather than a stack one — but it is still unbounded input driving
/// unbounded state, and it must complete.
#[test]
fn deeply_nested_rtf_groups_complete() {
    let mut v = b"{\\rtf1".to_vec();
    v.extend(std::iter::repeat_n(b'{', 500_000));
    v.extend_from_slice(b"text");
    on_small_stack("rtf-nest", move || {
        let _ = chunks_rs::get_chunks_from_bytes(&v, "probe.rtf", "default", 3, 1, 3, 15);
    });
}

/// A spreadsheet range reference is attacker-controlled and used to size
/// per-row allocations. `A1:AAAAAAAAAA1` asked for ~1.4e14 columns.
#[test]
fn implausible_spreadsheet_ranges_are_rejected() {
    use chunks_rs::formats::xlsx::common::{parse_range_ref, MAX_SHEET_COLS, MAX_SHEET_ROWS};
    assert!(
        parse_range_ref("A1:AAAAAAAAAA1").is_none(),
        "a 10-letter column must be rejected, not honoured"
    );
    assert!(
        parse_range_ref("A1:A99999999999").is_none(),
        "a row past the grid must be rejected"
    );
    // The real grid still works.
    assert_eq!(parse_range_ref("A1:B2"), Some((0, 0, 1, 1)));
    let (_, _, _, last_col) = parse_range_ref("A1:XFD1").expect("XFD is Excel's last column");
    assert_eq!(last_col, MAX_SHEET_COLS - 1);
    // Excel rows are 1-based, so row MAX_SHEET_ROWS is the LAST valid one
    // (index MAX_SHEET_ROWS - 1) and must still be accepted; one past it is not.
    assert!(
        parse_range_ref(&format!("A1:A{MAX_SHEET_ROWS}")).is_some(),
        "the last real row must still parse"
    );
    assert!(
        parse_range_ref(&format!("A1:A{}", MAX_SHEET_ROWS + 1)).is_none(),
        "a row past the grid must be rejected"
    );
}

/// `col_letter_to_index` underflowed on an empty label and overflowed on a long
/// one; both are reachable from a crafted `ref=`.
#[test]
fn column_labels_never_overflow_or_underflow() {
    use chunks_rs::formats::xlsx::common::{col_letter_to_index, MAX_SHEET_COLS};
    assert_eq!(col_letter_to_index("A"), 0);
    assert_eq!(col_letter_to_index("B"), 1);
    assert_eq!(col_letter_to_index("AA"), 26);
    assert_eq!(col_letter_to_index(""), 0, "empty label must not underflow");
    // One past the grid is the deliberate sentinel: it keeps the value bounded
    // AND makes `parse_range_ref` reject the reference rather than clamp it.
    let absurd = col_letter_to_index("AAAAAAAAAAAAAAAAAAAA");
    assert_eq!(absurd, MAX_SHEET_COLS, "must be the out-of-grid sentinel");
}
