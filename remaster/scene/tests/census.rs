//! LT-118's criteria: research 16 §4.5 lists every 3D draw path, and the code agrees.
//!
//! @LT-118/1 needs the disc: `LT_DISC=<…bin> cargo test --release -p lt-scene --test census -- --ignored`.

use std::path::{Path, PathBuf};

use lt_scene::census::{Census, Code};

fn doc() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/research/16-native-remaster.md")).expect("research 16")
}

/// §4.5's text, up to the next section.
fn section() -> String {
    let d = doc();
    let start = d.find("### 4.5 Every 3D draw path").expect("§4.5");
    let rest = &d[start..];
    rest[..rest.find("\n## 5.").unwrap_or(rest.len())].to_string()
}

/// The rows of the markdown table whose header starts with `header`.
fn table(section: &str, header: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = section.lines().collect();
    let at = lines.iter().position(|l| l.starts_with(header)).unwrap_or_else(|| panic!("no table {header}"));
    lines[at + 2..].iter().take_while(|l| l.starts_with('|')).map(|l| l.trim_matches('|').split(" | ").map(|c| c.trim().to_string()).collect()).collect()
}

/// @LT-118/1: §4.5 lists every function the code projects with, and every 3D path through them,
/// each with its function, what it is handed, and whether A1 captures it. The list is checked
/// against the disc: the functions holding RTPS/RTPT are exactly the ones the doc names, and
/// each projecting function is named on a path (or as unused).
#[test]
#[ignore = "needs LT_DISC"]
fn lt118_1_every_3d_draw_path_is_listed_with_what_a1_captures() {
    let disc = PathBuf::from(std::env::var("LT_DISC").expect("set LT_DISC"));
    let d = lt_core::DiscImage::open(&disc).unwrap();
    let exe = lt_core::PsxExe::from_disc(&d).unwrap();
    let table_ = lt_core::FileTable::parse(&exe, d.sectors()).unwrap();
    let wd = lt_core::worldmap::WorldDisc { image: &d, table: &table_, exe: &exe };
    let code = Code::new(exe.clone(), |id| wd.file(id).ok());
    let c = Census::of(&code);
    let s = section();
    let functions = table(&s, "| Function | Unit | GTE |");
    let named: String = functions.iter().map(|r| r[0].clone()).collect::<Vec<_>>().join(" ");
    assert_eq!(c.functions.len(), 12, "the functions that project");
    for f in &c.functions {
        assert!(named.contains(&format!("`{}`", f.symbol.name)), "§4.5's function table names {}", f.symbol.name);
    }
    assert_eq!(c.stubs.iter().map(|(s, _)| s.name).collect::<Vec<_>>(), ["FUN_8019dd68"]);
    let paths = table(&s, "| # | Path |");
    assert!(paths.len() >= 15, "{} paths", paths.len());
    for r in &paths {
        assert_eq!(r.len(), 7, "every path has its function, what it's handed, calls, capture and plan: {r:?}");
        assert!(!r[3].is_empty() && !r[5].is_empty(), "{r:?}");
    }
    let drawn: String = paths.iter().map(|r| r[2].clone()).collect::<Vec<_>>().join(" ");
    for f in &c.functions {
        assert!(drawn.contains(f.symbol.name), "a path draws with {}", f.symbol.name);
    }
    // What the paths say is captured is what the capture hooks.
    let hooked: Vec<String> = lt_scene::hooks().hooks().iter().map(|h| h.name.clone()).collect();
    for name in ["WorldChunk_DrawObjectMesh", "Npc_DrawIfVisible", "Model_DrawParts (second entry)", "Field_DrawTorchFlames", "FUN_8014c97c (OVR03 lit arena)"] {
        assert!(hooked.iter().any(|h| h == name), "A1 hooks {name}");
    }
    // Rows 1 and 2, row 8 since LT-142 (the event tasks' actors), row 9 since LT-143 (the first
    // flight's sky), rows 10 and 11 since LT-116 (battles' actors and arena).
    assert!(paths.iter().filter(|r| r[5].starts_with("Yes")).count() == 6, "rows 1, 2, 8, 9, 10 and 11 are captured");
}

/// @LT-118/2: each path A1 doesn't capture has a ticket or a reason to stay on the emulated layer.
#[test]
fn lt118_2_each_uncaptured_path_has_a_ticket_or_a_reason() {
    let s = section();
    let paths = table(&s, "| # | Path |");
    let ticket = regex_lite_ticket;
    let mut tickets = Vec::new();
    for r in &paths {
        let (captured, plan) = (&r[5], &r[6]);
        if captured.starts_with("Yes") {
            assert!(plan.starts_with("Native"), "a captured path is drawn natively: {r:?}");
            continue;
        }
        let keys = ticket(plan);
        assert!(!keys.is_empty() || plan.starts_with("Reason:"), "path {} ({}) has a ticket or a reason: {plan}", r[0], r[1]);
        tickets.extend(keys);
    }
    tickets.sort();
    tickets.dedup();
    // LT-142's and LT-143's rows are captured now; LT-116 names what battles leave to the 2D layer
    // (blob shadows, effects, the transition's swirl).
    assert_eq!(tickets, ["LT-116", "LT-126", "LT-144"]);
}

/// The `LT-n` keys in `s`.
fn regex_lite_ticket(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(at) = s[i..].find("LT-") {
        let start = i + at + 3;
        let end = start + b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
        if end > start {
            out.push(s[i + at..end].to_string());
        }
        i = end.max(start);
    }
    out
}
