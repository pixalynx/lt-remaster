//! The world map's picture (`sys/16`, and `sys/17` and `sys/18` with the
//! location pins, which the Mapster toy opens) and the field's minimap (the
//! same picture, in the HUD atlas `sys/2` at VRAM (960, 0)): new land drawn
//! into them, in their own colours, so an island a project raises shows on
//! both.
//!
//! The picture is a 256 × 256 4bpp TIM of the whole overworld, north up,
//! 2.25 world tiles (of 128 units) to a pixel; `examples/worldmap_fit.rs`
//! fits it to the chunks' land and sea: 96.3 % of its pixels agree with the
//! world's tiles at an offset of (7, 7) pixels, unflipped. Its sea is index
//! 14 with a lighter coastal blue (13); lowland is green (8), woods a darker
//! green (6), sand and hills tans and browns.
//!
//! [`redraw`] works from retail's picture every time: a pixel most of whose
//! tiles are new land takes that land's colour, sea beside new land turns
//! coastal, and every other pixel is retail's. Only retail sea pixels change,
//! so the pins (drawn over land) stay, and taking the land away gives the
//! picture back byte for byte.

use super::ground::Material;
use super::land::{LandMap, LandTile};
use super::{EditError, Result};

/// The pictures: (file, bytes a pixel row takes, byte where the picture starts in a row). The world map without
/// pins and its two pin variants; the HUD atlas, whose image is 512 pixels wide with the minimap in its east half.
pub const FILES: [(&str, usize, usize); 4] = [("sys/16", 128, 0), ("sys/17", 128, 0), ("sys/18", 128, 0), ("sys/2", 256, 128)];

/// World tiles per side.
const WORLD: i32 = 576;
/// Where world tile (0, 0) (the north-west of grid chunk 0) falls on the picture.
const OFFSET: i32 = 7;
const SEA: u8 = 14;
const COAST: u8 = 13;
const GRASS: u8 = 8;
const WOODS: u8 = 6;
const SAND: u8 = 5;
const HILL: u8 = 4;
const HIGH: u8 = 3;

/// The grid chunk and its tile (column, row from the south-west) at world tile (wx, wz), north-up rows.
pub fn tile_at(wx: i32, wz: i32) -> Option<(u32, usize, usize)> {
    let (wx, wz) = (wx.rem_euclid(WORLD), wz.rem_euclid(WORLD));
    let gy = (wz + 16).div_euclid(32).rem_euclid(18);
    let row = (32 * gy + 15 - wz).rem_euclid(WORLD);
    for gx in [(wx + 16).div_euclid(16), (wx + 16).div_euclid(16) - 1] {
        if gx.rem_euclid(2) != gy % 2 {
            continue;
        }
        let col = (wx - 16 * gx + 16).rem_euclid(WORLD);
        if col < 32 && row < 32 {
            let id = crate::world::chunk_at(gx.rem_euclid(36) as u32, gy as u32)?;
            return Some((id, col as usize, row as usize));
        }
    }
    None
}

/// The colour a tile of new land takes.
fn colour(t: LandTile) -> u8 {
    match Material::from_id(t.material) {
        Some(Material::Water) => COAST,
        Some(Material::Sand) => SAND,
        Some(Material::Forest) => WOODS,
        _ if t.level >= 4 => HIGH,
        _ if t.level >= 2 => HILL,
        _ => GRASS,
    }
}

/// Offset of the picture's first pixel in a 4bpp TIM with a CLUT (header, CLUT block, pixel block header).
fn pixels(tim: &[u8], row: usize, x0: usize) -> Result<usize> {
    let bad = || EditError::new("MAP_TIM", "The world map picture isn't the 256 × 256 4bpp TIM retail has.");
    if tim.len() < 20 || u32::from_le_bytes(tim[0..4].try_into().unwrap()) != 0x10 {
        return Err(bad());
    }
    let at = 8 + u32::from_le_bytes(tim[8..12].try_into().unwrap()) as usize + 12 + x0;
    (tim.len() >= at + row * 255 + 128).then_some(at).ok_or_else(bad)
}

/// The picture with the land of `land` (a chunk's map, by chunk) drawn over retail's sea. `row` and `x0` place the
/// picture in the TIM ([`FILES`]).
pub fn redraw(retail: &[u8], row: usize, x0: usize, land: &dyn Fn(u32) -> Option<std::rc::Rc<LandMap>>) -> Result<Vec<u8>> {
    let at = pixels(retail, row, x0)?;
    let mut out = retail.to_vec();
    let get = |b: &[u8], x: i32, y: i32| {
        let (x, y) = (x.rem_euclid(256) as usize, y.rem_euclid(256) as usize);
        (b[at + y * row + x / 2] >> ((x & 1) * 4)) & 0xF
    };
    let set = |b: &mut [u8], x: usize, y: usize, v: u8| {
        let i = at + y * row + x / 2;
        b[i] = if x & 1 == 0 { (b[i] & 0xF0) | v } else { (b[i] & 0x0F) | (v << 4) };
    };
    // Each pixel's new land: the colour most of its tiles take, when they are most of it.
    let mut drawn = vec![None::<u8>; 256 * 256];
    for py in 0..256i32 {
        for px in 0..256i32 {
            let (mx, my) = ((px - OFFSET).rem_euclid(256), (py - OFFSET).rem_euclid(256));
            let (x0, x1) = ((mx * 9) / 4, ((mx + 1) * 9 + 3) / 4);
            let (z0, z1) = ((my * 9) / 4, ((my + 1) * 9 + 3) / 4);
            let (mut votes, mut total) = ([0u32; 16], 0u32);
            for wz in z0..z1 {
                for wx in x0..x1 {
                    total += 1;
                    let Some((chunk, col, row)) = tile_at(wx, wz) else { continue };
                    if let Some(t) = land(chunk).and_then(|m| m.get(col as i32, row as i32)).filter(|t| t.painted()) {
                        votes[colour(t) as usize] += 1;
                    }
                }
            }
            let new = votes.iter().sum::<u32>();
            if new * 2 >= total.max(1) {
                drawn[(py * 256 + px) as usize] = (0..16u8).max_by_key(|&v| votes[v as usize]);
            }
        }
    }
    for py in 0..256 {
        for px in 0..256 {
            let was = get(retail, px, py);
            if was != SEA && was != COAST {
                continue;
            }
            let i = (py * 256 + px) as usize;
            let v = match drawn[i] {
                Some(v) => v,
                // Sea beside new land turns coastal.
                None if was == SEA && (-1..=1).any(|dy| (-1..=1).any(|dx| drawn[((py + dy).rem_euclid(256) * 256 + (px + dx).rem_euclid(256)) as usize].is_some_and(|v| v != COAST))) => COAST,
                None => was,
            };
            set(&mut out, px as usize, py as usize, v);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn picture() -> Vec<u8> {
        // A 4bpp TIM: header, a 16-colour CLUT block, a 64 × 256 halfword block of sea.
        let mut t = vec![0x10, 0, 0, 0, 8, 0, 0, 0];
        t.extend_from_slice(&(12u32 + 32).to_le_bytes());
        t.extend_from_slice(&[16, 0, 252, 1, 16, 0, 1, 0]);
        t.extend(std::iter::repeat(0).take(32));
        t.extend_from_slice(&(12u32 + 128 * 256).to_le_bytes());
        t.extend_from_slice(&[192, 3, 0, 0, 64, 0, 0, 1]);
        t.extend(std::iter::repeat((SEA << 4) | SEA).take(128 * 256));
        t
    }

    #[test]
    fn world_tiles_belong_to_their_chunks() {
        // Grid chunk 0's tile (0, 31) (north-west) is world tile (wx −16 → 560, wz 32·0 + 15 − 31 → 560).
        assert_eq!(tile_at(560, 560), Some((0, 0, 31)));
        assert_eq!(tile_at(15, 15), Some((0, 31, 0)));
        // Chunk 136 is grid (28, 6): its tile (5, 7) sits at wx 16·28 − 16 + 5, wz 32·6 + 15 − 7.
        assert_eq!(tile_at(16 * 28 - 16 + 5, 32 * 6 + 15 - 7), Some((136, 5, 7)));
    }

    #[test]
    fn new_land_is_drawn_and_taken_back() {
        let retail = picture();
        let mut m = LandMap::default();
        for r in 8..24 {
            for c in 8..24 {
                m.tiles[r * 32 + c] = LandTile { material: Material::Grass as u8, level: 0, ramp: 0, drop: 0 };
            }
        }
        let m = Rc::new(m);
        let out = redraw(&retail, 128, 0, &|c| (c == 136).then(|| m.clone())).unwrap();
        assert_ne!(out, retail);
        let at = pixels(&out, 128, 0).unwrap();
        let count = |v: u8| (0..256 * 256).filter(|i| ((out[at + i / 2] >> ((i & 1) * 4)) & 0xF) == v).count();
        // 16 × 16 tiles ≈ 7 × 7 pixels of grass, ringed with coast.
        assert!((36..=64).contains(&count(GRASS)), "{}", count(GRASS));
        assert!(count(COAST) > 20);
        assert_eq!(redraw(&retail, 128, 0, &|_| None).unwrap(), retail);
    }
}
