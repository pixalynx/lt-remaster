//! @LT-117/3: Paper's screen 22 shows the Remastered switch and the native window's toolbar, light
//! and dark. The artboards are exported here (`app/paper/`, listed in its README) from the Paper
//! file "Living Tool 2", page "22 — Play"; this checks the four are there at their artboards' sizes
//! and that the README names each one.

use std::path::Path;

/// A WebP's canvas size (lossy `VP8 `, lossless `VP8L` or extended `VP8X`).
fn webp_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WEBP" {
        return None;
    }
    let u24 = |at: usize| u32::from(b[at]) | u32::from(b[at + 1]) << 8 | u32::from(b[at + 2]) << 16;
    match b.get(12..16)? {
        b"VP8X" => Some((u24(24) + 1, u24(27) + 1)),
        b"VP8 " => {
            let w = u16::from_le_bytes([b[26], b[27]]) & 0x3FFF;
            let h = u16::from_le_bytes([b[28], b[29]]) & 0x3FFF;
            Some((u32::from(w), u32::from(h)))
        }
        b"VP8L" => {
            let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        _ => None,
    }
}

#[test]
fn lt117_3_paper_screen_22_shows_the_switch_and_the_window_light_and_dark() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("paper");
    let readme = std::fs::read_to_string(dir.join("README.md")).expect("paper/README.md");
    for (file, artboard, size) in [
        ("22-play-remastered.webp", "22 — Play · Remastered", (1440, 960)),
        ("22-play-remastered-dark.webp", "22 — Play · Remastered", (1440, 960)),
        ("22-remastered-window.webp", "22 — Remastered window", (1280, 788)),
        ("22-remastered-window-dark.webp", "22 — Remastered window", (1280, 788)),
    ] {
        let bytes = std::fs::read(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(webp_size(&bytes), Some(size), "{file} is the artboard's export");
        assert!(readme.contains(artboard), "the README names {artboard}");
    }
    for part in ["Look, Lights, Aspect, Frame rate, Distance", "the switch", "· Dark"] {
        assert!(readme.contains(part), "the README says what they show: {part}");
    }
}
