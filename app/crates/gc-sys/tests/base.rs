//! A HackerGC base whose main moved (LT-104 criterion 4): gc-sys's addresses for it must be the
//! ones Living Tool already reads that base with (`lt_core::layout::Layout`, from the package's
//! `base.json`), and its moved globals must really have moved.
//!
//! ```text
//! LT_TEST_BASE=<base package folder, with base.json> cargo test -p gc-sys --test base -- --nocapture
//! ```
//!
//! Skipped (passes) without `LT_TEST_BASE`, as Living Tool's other base tests are.

use gc_sys::{Addrs, Unit, CHARACTERS, CHUNK_DATA, CHUNK_SLOTS, CTX, SYMBOLS};
use lt_core::layout::Layout;

#[test]
fn a_shifted_base_gives_the_moved_addresses() {
    let Some(dir) = std::env::var_os("LT_TEST_BASE") else {
        eprintln!("LT_TEST_BASE not set: skipped");
        return;
    };
    let path = std::path::Path::new(&dir).join("base.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).expect("base.json")).expect("json");
    let layout: Layout = serde_json::from_value(json["profile"]["exeLayout"].clone()).expect("profile.exeLayout");
    assert!(!layout.ranges.is_empty(), "this base's main didn't move");
    let addrs = Addrs::with_ranges(&layout.ranges);

    // Every global and every one of main's functions lands where Living Tool reads it.
    let globals = [CTX, CHARACTERS, CHUNK_SLOTS, CHUNK_DATA];
    for g in globals {
        assert_eq!(addrs.of(g, 0), layout.map(g.addr), "{}", g.name);
        println!("{:<12} retail {:#010x} -> {:#010x}", g.name, g.addr, addrs.of(g, 0));
    }
    let mut moved_functions = 0;
    for s in SYMBOLS.iter().filter(|s| s.unit == Unit::Main) {
        let here = addrs.function(s.name).expect("by name");
        assert_eq!(here, layout.map(s.addr), "{}", s.name);
        moved_functions += usize::from(here != s.addr);
    }
    println!("main functions moved: {moved_functions}");

    // Main's own data moved; the scratchpad and the fixed buffers below main didn't.
    assert_ne!(addrs.of(CHUNK_SLOTS, 0), CHUNK_SLOTS.addr, "the chunk slot table is in main's BSS");
    assert_eq!(addrs.of(CTX, 0), CTX.addr);
    assert_eq!(addrs.of(CHARACTERS, 0), CHARACTERS.addr);
    assert_eq!(addrs.of(CHUNK_DATA, 3), CHUNK_DATA.addr + 3 * CHUNK_DATA.stride);
    assert!(moved_functions > 100, "main's code moved too");
    // An overlay's functions never move.
    assert_eq!(addrs.function("Camera_UpdateTask"), Some(0x801F_D83C));
}
