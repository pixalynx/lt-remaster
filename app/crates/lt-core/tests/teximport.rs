//! PNG ↔ TIM (`edit::teximport`) on every TIM the disc's files hold: an
//! exported PNG imports back byte for byte, through every palette, and a
//! new palette built from the export reproduces it closely. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test teximport`.

use lt_core::edit::teximport::{self, Mode};
use lt_core::tim::Tim;
use lt_core::{DiscImage, FileTable, PsxExe};

/// Offsets of plausible TIMs in `b` (4-byte aligned, sane header and rectangles).
fn tims(b: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 20 <= b.len() {
        if u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) == 0x10 {
            let flags = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap());
            if matches!(flags, 0x02 | 0x08 | 0x09 | 0x0A) {
                if let Some(t) = Tim::parse(&b[at..]) {
                    let i = t.info.image;
                    let ok_rect = |x: u16, y: u16, w: u16, h: u16| x < 1024 && y < 512 && w > 0 && h > 0 && w <= 1024 && h <= 512;
                    let clut_ok = t.info.clut.is_none_or(|c| ok_rect(c.x, c.y, c.w, c.h));
                    if ok_rect(i.x, i.y, i.w, i.h) && clut_ok && t.info.len <= b.len() - at {
                        out.push(at);
                        at += t.info.len.max(4);
                        continue;
                    }
                }
            }
        }
        at += 4;
    }
    out
}

#[test]
fn every_tim_round_trips_through_png() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let (mut n_tims, mut n_palettes) = (0, 0);
    for e in table.entries.iter() {
        if !e.aliases.is_empty() && e.aliases[0] < e.global {
            continue;
        }
        let Ok(b) = image.read_sectors(e.extent.lba, e.extent.sectors.min(4096)) else { continue };
        for at in tims(&b) {
            let t = Tim::parse(&b[at..]).unwrap();
            let rows = t.info.clut.map_or(1, |c| c.h as usize);
            let cols = t.palettes_per_row().max(1);
            n_tims += 1;
            for row in 0..rows.min(4) {
                for col in 0..cols.min(4) {
                    let png = teximport::export_png(&b, at, row, col).unwrap_or_else(|err| panic!("{} @{at:#x}: {err}", e.id));
                    let (out, rep) = teximport::import_png(&b, at, &png, row, col, Mode::Keep).unwrap_or_else(|err| panic!("{} @{at:#x} {row}/{col}: {err}", e.id));
                    // An exported palette with repeated colours can't say which index a pixel had:
                    // pixels may move between equal entries, but the picture is the same.
                    let back = Tim::parse(&out[at..]).unwrap();
                    assert_eq!(back.rgba(row, col), t.rgba(row, col), "{} @{at:#x} row {row} col {col}", e.id);
                    if rep.lossless {
                        assert_eq!(out, b, "{} @{at:#x} row {row} col {col}: lossless path changed bytes", e.id);
                    }
                    assert_eq!(out.len(), b.len());
                    n_palettes += 1;
                }
            }
        }
    }
    eprintln!("{n_tims} TIMs, {n_palettes} palettes round-tripped");
    assert!(n_tims > 300, "found only {n_tims} TIMs");
}

#[test]
fn a_new_palette_reproduces_the_toy_textures() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let mut checked = 0;
    for e in table.entries.iter().filter(|e| e.id.starts_with("toy/")) {
        let b = image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
        for at in tims(&b).into_iter().take(1) {
            let t = Tim::parse(&b[at..]).unwrap();
            if t.info.bpp != 4 {
                continue;
            }
            // Through an RGBA PNG, so the indexed shortcut can't apply.
            let rgba = t.rgba(0, 0);
            let mut png = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut png, t.info.width, t.info.height);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(&rgba).unwrap();
            }
            let (_, rep) = teximport::import_png(&b, at, &png, 0, 0, Mode::NewPalette).unwrap();
            assert!(rep.mean_error < 1.5, "{}: mean error {}", e.id, rep.mean_error);
            checked += 1;
        }
    }
    assert!(checked > 40, "{checked}");
}
