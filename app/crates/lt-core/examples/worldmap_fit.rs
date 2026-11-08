//! Fits the game's world map picture (`sys/16`, 256 × 256, 4bpp) to the
//! overworld's land and sea, tile by tile: which way it faces and where the
//! world's origin falls on it. The result is `edit::map_screen`'s projection
//! (offset (7, 7), unflipped).
//!
//! `cargo run --release -p lt-core --example worldmap_fit -- "<Guardian's Crusade.bin>"`

use lt_core::edit::chunk::Chunk;
use lt_core::edit::land;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

/// World tiles per side (36 columns × 16 tiles = 18 rows × 32 tiles = 576).
const N: usize = 576;

fn main() {
    let path = std::env::args().nth(1).expect("the retail .bin");
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    // Land tiles, north up: row 0 of the mask is the far north (world z = +2048).
    let mut world = vec![0u8; N * N];
    for id in 0..324u32 {
        let (gx, gy) = lt_core::world::grid_cell(id).unwrap();
        let c = Chunk::parse(&w.chunk(id).unwrap()).unwrap();
        let sea = land::sea_tiles(&c);
        for row in 0..32 {
            for col in 0..32 {
                let wx = (16 * gx as usize + N - 16 + col) % N;
                let wz = (32 * gy as usize + 15 - row) % N;
                world[wz * N + wx] = !sea[row * 32 + col] as u8;
            }
        }
    }
    // The map without pins.
    let tim = w.file("sys/16").unwrap();
    let t = lt_core::tim::Tim::parse(&tim).unwrap();
    println!("sys/16: image {:?}, clut {:?}", t.info.image, t.info.clut);
    let pix = 8 + u32::from_le_bytes(tim[8..12].try_into().unwrap()) as usize + 12;
    let index = |x: usize, y: usize| (tim[pix + y * 128 + x / 2] >> ((x & 1) * 4)) & 0xF;
    let mut count = [0usize; 16];
    for y in 0..256 {
        for x in 0..256 {
            count[index(x, y) as usize] += 1;
        }
    }
    // The palette: sea indices are the blue ones (blue well above red and green).
    let clut: Vec<u16> = (0..16).map(|i| u16::from_le_bytes([tim[20 + 2 * i], tim[21 + 2 * i]])).collect();
    let rgb = |c: u16| [(c & 31) as u32 * 8, ((c >> 5) & 31) as u32 * 8, ((c >> 10) & 31) as u32 * 8];
    let blue: Vec<bool> = clut.iter().map(|&c| {
        let [r, g, b] = rgb(c);
        b > r + 40 && b > g + 20
    }).collect();
    for i in 0..16 {
        println!("{i:2}: {:?} {:5} {}", rgb(clut[i]), count[i], if blue[i] { "sea" } else { "" });
    }
    let sea = |i: u8| blue[i as usize];
    let map: Vec<u8> = (0..256 * 256).map(|i| !sea(index(i % 256, i / 256)) as u8).collect();
    // The world mask at the map's resolution.
    let at = |x: usize, y: usize| world[((y * N) / 256) * N + (x * N) / 256];
    let score = |fx: bool, fy: bool, ox: usize, oy: usize, step: usize| {
        let mut s = 0;
        for y in (0..256).step_by(step) {
            for x in (0..256).step_by(step) {
                let (mx, my) = ((x + ox) % 256, (y + oy) % 256);
                let (mx, my) = (if fx { 255 - mx } else { mx }, if fy { 255 - my } else { my });
                s += (map[my * 256 + mx] == at(x, y)) as usize;
            }
        }
        s
    };
    let mut best = (0usize, false, false, 0usize, 0usize);
    for fx in [false, true] {
        for fy in [false, true] {
            for oy in (0..256).step_by(2) {
                for ox in (0..256).step_by(2) {
                    let s = score(fx, fy, ox, oy, 8);
                    if s > best.0 {
                        best = (s, fx, fy, ox, oy);
                    }
                }
            }
        }
    }
    let (_, fx, fy, ox0, oy0) = best;
    let mut fine = (0usize, ox0, oy0);
    for dy in 0..5 {
        for dx in 0..5 {
            let (ox, oy) = ((ox0 + 254 + dx) % 256, (oy0 + 254 + dy) % 256);
            let s = score(fx, fy, ox, oy, 1);
            if s > fine.0 {
                fine = (s, ox, oy);
            }
        }
    }
    let best = (fine.0, fx, fy, fine.1, fine.2);
    // The full score at the best offset.
    let (_, fx, fy, ox, oy) = best;
    let mut s = 0;
    for y in 0..256 {
        for x in 0..256 {
            let (mx, my) = ((x + ox) % 256, (y + oy) % 256);
            let (mx, my) = (if fx { 255 - mx } else { mx }, if fy { 255 - my } else { my });
            s += (map[my * 256 + mx] == at(x, y)) as usize;
        }
    }
    println!("flip x {fx}, flip y {fy}, offset ({ox}, {oy}): {:.1}% of pixels agree", s as f64 / 655.36);
}
