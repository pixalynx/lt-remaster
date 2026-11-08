//! Modular log houses for the construction kit (screen 10): parts made from
//! the village's own textures (chunk 78's page) on the 128-unit module the
//! retail houses are built on, and a builder that puts them together.
//!
//! The retail log houses (chunk 78) measure in 128-unit modules (4 cells):
//! walls are 128-wide quads two log rows high (64 units each), a 32-unit
//! trim band runs along the top, and the roof rises 64 over each 128-unit
//! run from the trim (160 above the ground) to a ridge 224 up, two modules
//! deep with gable triangles at the ends. Each texture tile maps 2 units to
//! a texel, as there (`docs/research/05-props.md`).
//!
//! Every part is a 4 × 4-cell tile with its piece on one edge, so parts
//! snap on a 4-cell grid and turn into place: a wall stands on the tile's
//! south edge (turned 90° it stands on the east edge, 180° north, 270°
//! west), a roof slope rises from the south edge to a ridge on the north
//! edge, and the two gable halves stand on the west edge. A wall blocks the
//! row of cells it stands on.
//!
//! Faces follow the retail winding: as seen from the visible side, corners
//! go top-left, top-right, bottom-left, bottom-right.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::chunk::{Chunk, Texture};
use super::prefab::{self, CellRect, PatchCell, Prefab, PrefabPrim, PrimData, Tile, TileOrigin};
use super::{EditError, Result};

/// Where the textures come from: the Knight's village.
pub const SOURCE_CHUNK: u32 = 78;

/// Heights above the ground (units).
pub(super) const ROW: i16 = 64;
pub(super) const WALL: i16 = 128;
pub(super) const EAVE: i16 = 160;
pub(super) const RIDGE: i16 = 224;
const CHIMNEY_TOP: i16 = 256;
/// One module.
pub(super) const M: i16 = 128;

/// Roof colours of the village houses: the roof, trim and chimney palettes
/// (the logs are 0x7806 in all of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Roof {
    Orange,
    Blue,
    Red,
}

impl Roof {
    pub const ALL: [Roof; 3] = [Roof::Orange, Roof::Blue, Roof::Red];

    pub fn key(self) -> &'static str {
        match self {
            Roof::Orange => "orange",
            Roof::Blue => "blue",
            Roof::Red => "red",
        }
    }

    pub fn from_key(k: &str) -> Option<Roof> {
        Roof::ALL.into_iter().find(|r| r.key() == k)
    }

    /// Roof, trim, chimney CLUTs in chunk 78.
    pub(super) fn cluts(self) -> (u16, u16, u16) {
        match self {
            Roof::Orange => (0x7806, 0x7886, 0x7846),
            Roof::Blue => (0x7A46, 0x7A46, 0x7A86),
            Roof::Red => (0x7A06, 0x7A06, 0x7846),
        }
    }
}

pub(super) const LOGS: u16 = 0x7806;

/// Texture rectangles on chunk 78's page, `[x0, y0, x1, y1]` inclusive.
pub(super) mod tex {
    pub const LOGS_PLAIN: [u16; 4] = [0, 0, 63, 31];
    /// Log ends on the right.
    pub const LOGS_END: [u16; 4] = [0, 32, 63, 63];
    /// Log ends on both sides.
    pub const LOGS_BOTH: [u16; 4] = [0, 64, 63, 95];
    /// A window on the left, log ends on the right.
    pub const WINDOW: [u16; 4] = [0, 96, 63, 127];
    pub const TRIM: [u16; 4] = [0, 160, 63, 175];
    /// Right-angled triangle, hypotenuse from (0, 207) up to (63, 176).
    pub const GABLE: [u16; 4] = [0, 176, 63, 207];
    pub const ROOF: [u16; 4] = [64, 0, 127, 63];
    /// A column of log ends (the door's sides).
    pub const JAMB: [u16; 4] = [32, 224, 47, 255];
    pub const STEP: [u16; 4] = [64, 72, 87, 79];
    pub const BRICK: [u16; 4] = [128, 0, 159, 47];
    pub const FLUE: [u16; 4] = [0, 224, 31, 255];
}

/// How a wall's log ends sit, as seen from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ends {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallKind {
    Logs(Ends),
    /// Logs below, a window in the upper row (on the right when the ends are on the left).
    Window(Ends),
    Door,
}

/// Brightness baked into each face (128 = full), by the way it faces once
/// built, like the retail houses' lighting.
#[derive(Debug, Clone, Copy)]
pub struct Shade(pub u8);

pub(super) const SOUTH: Shade = Shade(88);
pub(super) const NORTH: Shade = Shade(64);
pub(super) const WEST: Shade = Shade(110);
pub(super) const EAST: Shade = Shade(72);
pub(super) const ROOF_SOUTH: Shade = Shade(150);
pub(super) const ROOF_NORTH: Shade = Shade(100);

/// A rectangle of the page pasted into a tile, at a texel column: a window in a wall.
pub(super) type Paste = ([u16; 4], u16);

/// First id [`Builder::custom`] hands out for a palette of the design's own.
const CUSTOM_CLUT: u16 = 0xF000;

/// Collects a prefab's geometry and its texture tiles (cut from the page once each).
pub(super) struct Builder<'a> {
    page: &'a Texture,
    pub(super) pool: Vec<[i16; 3]>,
    index: HashMap<[i16; 3], u16>,
    pub(super) prims: Vec<PrefabPrim>,
    pub(super) tiles: Vec<Tile>,
    /// (page's VRAM x, y; rectangle; CLUT id; what is pasted into it) → tile.
    tile_of: HashMap<(u16, u16, [u16; 4], u16, Option<Paste>), u16>,
    /// Added to every vertex (x, height, z) before it is stored.
    pub(super) shift: [i16; 3],
    /// Palettes of the design's own: (the retail CLUT the texels were cut with, the palette).
    customs: Vec<(u16, Vec<u16>)>,
}

impl<'a> Builder<'a> {
    pub(super) fn new(page: &'a Texture) -> Self {
        Builder { page, pool: Vec::new(), index: HashMap::new(), prims: Vec::new(), tiles: Vec::new(), tile_of: HashMap::new(), shift: [0; 3], customs: Vec::new() }
    }

    /// Cuts the next tiles from `page` instead (a room's finishes come from several interiors' pages).
    pub(super) fn set_page(&mut self, page: &'a Texture) {
        self.page = page;
    }

    /// A CLUT id that draws like `base` with `palette`'s colours instead (16 BGR555 entries).
    pub(super) fn custom(&mut self, base: u16, palette: &[u16]) -> u16 {
        let k = match self.customs.iter().position(|(b, p)| *b == base && p == palette) {
            Some(k) => k,
            None => {
                self.customs.push((base, palette.to_vec()));
                self.customs.len() - 1
            }
        };
        CUSTOM_CLUT + k as u16
    }

    pub(super) fn vertex(&mut self, v: [i16; 3]) -> u16 {
        let v = [v[0] + self.shift[0], v[1] + self.shift[1], v[2] + self.shift[2]];
        // Prefab pools are Y down from the ground: heights go negative.
        let v = [v[0], -v[1], v[2]];
        *self.index.entry(v).or_insert_with(|| {
            self.pool.push(v);
            (self.pool.len() - 1) as u16
        })
    }

    fn tile(&mut self, rect: [u16; 4], clut: u16) -> Result<u16> {
        self.tile_with(rect, clut, None)
    }

    /// The tile of `rect` through `clut`, with `paste` (another rectangle of the
    /// page, and the column it goes at) copied over it: a window in a wall.
    pub(super) fn tile_with(&mut self, rect: [u16; 4], clut: u16, paste: Option<Paste>) -> Result<u16> {
        let key = (self.page.image.x, self.page.image.y, rect, clut, paste);
        if let Some(&k) = self.tile_of.get(&key) {
            return Ok(k);
        }
        // One tile per (page, rectangle, CLUT id): a custom id stands for one (base, palette) pair.
        let (own, clut) = match clut.checked_sub(CUSTOM_CLUT).and_then(|k| self.customs.get(k as usize)) {
            Some((base, p)) => (Some(p.clone()), *base),
            None => (None, clut),
        };
        let t = self.page;
        let (w, h) = (rect[2] - rect[0] + 1, rect[3] - rect[1] + 1);
        let mut pixels = String::with_capacity(w as usize * h as usize);
        let texel = |x: u16, y: u16| {
            let b = t.pixels[y as usize * t.image.w as usize * 2 + x as usize / 2];
            if x & 1 == 0 {
                b & 15
            } else {
                b >> 4
            }
        };
        for y in rect[1]..=rect[3] {
            for x in rect[0]..=rect[2] {
                let v = match paste {
                    Some(([sx0, sy0, sx1, sy1], at)) if (rect[0] + at..=rect[0] + at + sx1 - sx0).contains(&x) && y - rect[1] <= sy1 - sy0 => texel(sx0 + x - rect[0] - at, sy0 + y - rect[1]),
                    _ => texel(x, y),
                };
                pixels.push(char::from_digit(v as u32, 16).unwrap());
            }
        }
        let (r, e) = t.clut.as_ref().ok_or_else(|| EditError::new("HOUSE_TEXTURE", "The page has no palettes."))?;
        let (row, col) = match (((clut >> 6) as usize).checked_sub(r.y as usize), ((clut & 0x3F) as usize * 16).checked_sub(r.x as usize)) {
            (Some(row), Some(col)) => (row, col),
            _ => return Err(EditError::new("HOUSE_TEXTURE", format!("CLUT {clut:#06x} isn't in the page's palettes."))),
        };
        let o = row * r.w as usize + col;
        let palette = match own {
            Some(p) if p.len() == 16 => p,
            Some(_) => return Err(EditError::new("HOUSE_PALETTE", "A palette of your own has 16 colours.")),
            None => e.get(o..o + 16).ok_or_else(|| EditError::new("HOUSE_TEXTURE", format!("CLUT {clut:#06x} isn't in the page.")))?.to_vec(),
        };
        // Where it is on the village's page: chunk 78 itself samples it there.
        let row = t.image.y + rect[1];
        let origin = paste.is_none().then_some(TileOrigin { page: t.image.x / 64 | ((row >> 8) & 1) << 4, x: rect[0], y: row & 0xFF, clut });
        self.tiles.push(Tile { w, h, pixels, palette, origin });
        let k = (self.tiles.len() - 1) as u16;
        self.tile_of.insert(key, k);
        Ok(k)
    }

    /// A textured quad: corners top-left, top-right, bottom-left, bottom-right
    /// as seen from its visible side (x, height, z), with the tile's UVs over
    /// `u0..=u1`, `v0..=v1` (relative to the tile).
    pub(super) fn quad(&mut self, c: [[i16; 3]; 4], rect: [u16; 4], clut: u16, uv: [[u8; 2]; 4], shade: Shade) -> Result<()> {
        let t = self.tile(rect, clut)?;
        let idx = [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[2]), self.vertex(c[3])];
        let rgb = [[shade.0; 3]; 4];
        self.prims.push(PrefabPrim { prim: PrimData { list: 7, code: 0x3C, n: 4, idx, rgb, uv, clut: 0, tpage: 0 }, tile: Some(t) });
        Ok(())
    }

    /// A textured quad with a colour per corner: flat (FT4) when they're all the same and `gouraud` is off,
    /// shaded (GT4) otherwise.
    pub(super) fn quad_rgb(&mut self, c: [[i16; 3]; 4], rect: [u16; 4], clut: u16, uv: [[u8; 2]; 4], rgb: [[u8; 3]; 4], gouraud: bool) -> Result<()> {
        let t = self.tile(rect, clut)?;
        let idx = [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[2]), self.vertex(c[3])];
        let (list, code, rgb) = if gouraud || rgb.iter().any(|v| *v != rgb[0]) { (7, 0x3C, rgb) } else { (3, 0x2C, [rgb[0]; 4]) };
        self.prims.push(PrefabPrim { prim: PrimData { list, code, n: 4, idx, rgb, uv, clut: 0, tpage: 0 }, tile: Some(t) });
        Ok(())
    }

    /// A textured quad on tile `t` (from [`Builder::tile_with`]): corners as [`Builder::quad`] takes them.
    pub(super) fn quad_on(&mut self, c: [[i16; 3]; 4], t: u16, uv: [[u8; 2]; 4], shade: Shade) {
        let idx = [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[2]), self.vertex(c[3])];
        self.prims.push(PrefabPrim { prim: PrimData { list: 7, code: 0x3C, n: 4, idx, rgb: [[shade.0; 3]; 4], uv, clut: 0, tpage: 0 }, tile: Some(t) });
    }

    /// A textured triangle on tile `t`.
    pub(super) fn tri_on(&mut self, c: [[i16; 3]; 3], t: u16, uv: [[u8; 2]; 3], shade: Shade) {
        let idx = [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[2]), 0];
        self.prims.push(PrefabPrim { prim: PrimData { list: 5, code: 0x34, n: 3, idx, rgb: [[shade.0; 3]; 4], uv: [uv[0], uv[1], uv[2], [0, 0]], clut: 0, tpage: 0 }, tile: Some(t) });
    }

    /// A quad showing the whole tile (mirrored left to right when `flip`).
    pub(super) fn panel(&mut self, c: [[i16; 3]; 4], rect: [u16; 4], clut: u16, flip: bool, shade: Shade) -> Result<()> {
        let (w, h) = ((rect[2] - rect[0]) as u8, (rect[3] - rect[1]) as u8);
        let (l, r) = if flip { (w, 0) } else { (0, w) };
        self.quad(c, rect, clut, [[l, 0], [r, 0], [l, h], [r, h]], shade)
    }

    pub(super) fn tri(&mut self, c: [[i16; 3]; 3], rect: [u16; 4], clut: u16, uv: [[u8; 2]; 3], shade: Shade) -> Result<()> {
        let t = self.tile(rect, clut)?;
        let idx = [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[2]), 0];
        let rgb = [[shade.0; 3]; 4];
        let uv = [uv[0], uv[1], uv[2], [0, 0]];
        self.prims.push(PrefabPrim { prim: PrimData { list: 5, code: 0x34, n: 3, idx, rgb, uv, clut: 0, tpage: 0 }, tile: Some(t) });
        Ok(())
    }

    /// An untextured flat quad (the dark inside of a doorway).
    pub(super) fn flat(&mut self, c: [[i16; 3]; 4], rgb: [u8; 3]) {
        let idx = [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[2]), self.vertex(c[3])];
        self.prims.push(PrefabPrim { prim: PrimData { list: 2, code: 0x28, n: 4, idx, rgb: [rgb; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 }, tile: None });
    }

    /// The prefab: a `w × h`-cell footprint on flat ground, `blocked` cells
    /// (column, row) closed to walking.
    fn finish(self, name: &str, w: u8, h: u8, blocked: &[(u8, u8)]) -> Prefab {
        let mut heights = vec![PatchCell::Rel(0); w as usize * h as usize];
        for &(c, r) in blocked {
            heights[r as usize * w as usize + c as usize] = PatchCell::Blocked;
        }
        Prefab {
            name: name.to_string(),
            source_chunk: SOURCE_CHUNK,
            area: CellRect { c0: 0, r0: 0, c1: w - 1, r1: h - 1 },
            ground_y: 0,
            pool: self.pool,
            prims: self.prims,
            tiles: self.tiles,
            heights,
            category: CATEGORY.to_string(),
            stock: None,
            stock_rev: 0,
            design: None,
        }
    }
}

pub const CATEGORY: &str = "House parts";

/// The log rows for a wall's ends, and whether to mirror them.
pub(super) fn logs(ends: Ends) -> ([u16; 4], bool) {
    match ends {
        Ends::None => (tex::LOGS_PLAIN, false),
        Ends::Right => (tex::LOGS_END, false),
        Ends::Left => (tex::LOGS_END, true),
        Ends::Both => (tex::LOGS_BOTH, false),
    }
}

/// A wall on the tile's south edge, outside to the south: two log rows and
/// the trim band. A door is an alcove 64 wide and 32 deep in the middle.
fn wall_into(b: &mut Builder, kind: WallKind, roof: Roof, shade: Shade) -> Result<()> {
    let (_, trim, _) = roof.cluts();
    let at = |x: i16, h: i16, z: i16| [x, h, z];
    let face = |x0: i16, x1: i16, h0: i16, h1: i16| [at(x0, h1, 0), at(x1, h1, 0), at(x0, h0, 0), at(x1, h0, 0)];
    match kind {
        WallKind::Door => {
            // Log strips either side of the opening, then the alcove.
            for (h0, h1) in [(0, ROW), (ROW, WALL)] {
                b.quad(face(0, 32, h0, h1), tex::LOGS_PLAIN, LOGS, [[0, 0], [15, 0], [0, 31], [15, 31]], shade)?;
                b.quad(face(96, M, h0, h1), tex::LOGS_PLAIN, LOGS, [[48, 0], [63, 0], [48, 31], [63, 31]], shade)?;
            }
            b.flat([at(32, WALL, 32), at(96, WALL, 32), at(32, 0, 32), at(96, 0, 32)], [14, 10, 8]);
            b.panel([at(32, WALL, 0), at(32, WALL, 32), at(32, 0, 0), at(32, 0, 32)], tex::JAMB, LOGS, false, Shade(shade.0.saturating_add(16)))?;
            b.panel([at(96, WALL, 32), at(96, WALL, 0), at(96, 0, 32), at(96, 0, 0)], tex::JAMB, LOGS, true, Shade(shade.0.saturating_sub(16)))?;
            b.panel([at(32, 1, 32), at(96, 1, 32), at(32, 1, 0), at(96, 1, 0)], tex::STEP, LOGS, false, Shade(102))?;
        }
        WallKind::Window(ends) => {
            let (rect, flip) = logs(ends);
            b.panel(face(0, M, 0, ROW), rect, LOGS, flip, shade)?;
            b.panel(face(0, M, ROW, WALL), tex::WINDOW, LOGS, ends == Ends::Left, shade)?;
        }
        WallKind::Logs(ends) => {
            let (rect, flip) = logs(ends);
            b.panel(face(0, M, 0, ROW), rect, LOGS, flip, shade)?;
            b.panel(face(0, M, ROW, WALL), rect, LOGS, flip, shade)?;
        }
    }
    b.panel(face(0, M, WALL, EAVE), tex::TRIM, trim, false, shade)
}

/// A roof slope over the tile: eave on the south edge (on the trim), ridge on the north edge.
fn roof_into(b: &mut Builder, roof: Roof, shade: Shade) -> Result<()> {
    let (clut, _, _) = roof.cluts();
    b.panel([[0, RIDGE, M], [M, RIDGE, M], [0, EAVE, 0], [M, EAVE, 0]], tex::ROOF, clut, false, shade)
}

/// Half a gable on the tile's west edge, rising from the eave at the south
/// edge (`a`) or the north edge (`b`) to the ridge on the other.
fn gable_into(b: &mut Builder, roof: Roof, south_eave: bool, shade: Shade) -> Result<()> {
    let (_, trim, _) = roof.cluts();
    let (eave, ridge) = if south_eave { (0, M) } else { (M, 0) };
    // Seen from the west, north is on the left.
    if south_eave {
        b.tri([[0, RIDGE, ridge], [0, EAVE, eave], [0, EAVE, ridge]], tex::GABLE, trim, [[63, 0], [0, 31], [63, 31]], shade)
    } else {
        b.tri([[0, EAVE, eave], [0, RIDGE, ridge], [0, EAVE, ridge]], tex::GABLE, trim, [[0, 31], [63, 0], [63, 31]], shade)
    }
}

/// A 64 × 64 brick chimney from the eave to 32 above the ridge.
pub(super) fn chimney_into(b: &mut Builder, roof: Roof) -> Result<()> {
    let (_, _, clut) = roof.cluts();
    let (lo, hi, w) = (EAVE, CHIMNEY_TOP, 64);
    let side = |b: &mut Builder, c: [[i16; 3]; 4], s: Shade| b.panel(c, tex::BRICK, clut, false, s);
    side(b, [[0, hi, 0], [w, hi, 0], [0, lo, 0], [w, lo, 0]], SOUTH)?;
    side(b, [[w, hi, 0], [w, hi, w], [w, lo, 0], [w, lo, w]], EAST)?;
    side(b, [[w, hi, w], [0, hi, w], [w, lo, w], [0, lo, w]], NORTH)?;
    side(b, [[0, hi, w], [0, hi, 0], [0, lo, w], [0, lo, 0]], WEST)?;
    b.panel([[0, hi, w], [w, hi, w], [0, hi, 0], [w, hi, 0]], tex::FLUE, clut, false, Shade(120))
}

/// The house parts in the library, each a 4 × 4-cell tile (the chimney 2 × 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Wall,
    WallWindow,
    WallDoor,
    WallEndLeft,
    WallEndRight,
    Roof,
    GableSouth,
    GableNorth,
    Chimney,
    /// Wall, roof and the west wall with its gable: a house's south-west corner.
    CornerWest,
    /// Wall, roof and the east wall with its gable: the south-east corner.
    CornerEast,
    /// Wall (with a window) and roof: a module along a house's side.
    Side,
    SideDoor,
}

impl Part {
    pub const ALL: [Part; 13] = [
        Part::CornerWest,
        Part::Side,
        Part::SideDoor,
        Part::CornerEast,
        Part::Wall,
        Part::WallWindow,
        Part::WallDoor,
        Part::WallEndLeft,
        Part::WallEndRight,
        Part::Roof,
        Part::GableSouth,
        Part::GableNorth,
        Part::Chimney,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Part::Wall => "wall",
            Part::WallWindow => "wall-window",
            Part::WallDoor => "wall-door",
            Part::WallEndLeft => "wall-end-left",
            Part::WallEndRight => "wall-end-right",
            Part::Roof => "roof",
            Part::GableSouth => "gable-a",
            Part::GableNorth => "gable-b",
            Part::Chimney => "chimney",
            Part::CornerWest => "corner-west",
            Part::CornerEast => "corner-east",
            Part::Side => "side",
            Part::SideDoor => "side-door",
        }
    }

    pub fn from_key(k: &str) -> Option<Part> {
        Part::ALL.into_iter().find(|p| p.key() == k)
    }

    pub fn name(self) -> &'static str {
        match self {
            Part::Wall => "Log wall",
            Part::WallWindow => "Log wall · window",
            Part::WallDoor => "Log wall · door",
            Part::WallEndLeft => "Log wall · corner left",
            Part::WallEndRight => "Log wall · corner right",
            Part::Roof => "Roof slope",
            Part::GableSouth => "Gable · rising north",
            Part::GableNorth => "Gable · rising south",
            Part::Chimney => "Chimney",
            Part::CornerWest => "House corner · west",
            Part::CornerEast => "House corner · east",
            Part::Side => "House side · window",
            Part::SideDoor => "House side · door",
        }
    }
}

/// One part in `roof` colours, textured from chunk 78 (`page` is its texture).
pub fn part(p: Part, roof: Roof, page: &Texture) -> Result<Prefab> {
    let mut b = Builder::new(page);
    let edge: &[(u8, u8)] = &[(0, 0), (1, 0), (2, 0), (3, 0)];
    let west: &[(u8, u8)] = &[(0, 0), (0, 1), (0, 2), (0, 3)];
    let east: &[(u8, u8)] = &[(3, 0), (3, 1), (3, 2), (3, 3)];
    let blocked: Vec<(u8, u8)> = match p {
        Part::Wall | Part::WallWindow | Part::WallDoor | Part::WallEndLeft | Part::WallEndRight => {
            let kind = match p {
                Part::WallWindow => WallKind::Window(Ends::None),
                Part::WallDoor => WallKind::Door,
                Part::WallEndLeft => WallKind::Logs(Ends::Left),
                Part::WallEndRight => WallKind::Logs(Ends::Right),
                _ => WallKind::Logs(Ends::None),
            };
            wall_into(&mut b, kind, roof, SOUTH)?;
            edge.to_vec()
        }
        Part::Roof => {
            roof_into(&mut b, roof, ROOF_SOUTH)?;
            Vec::new()
        }
        Part::GableSouth | Part::GableNorth => {
            gable_into(&mut b, roof, p == Part::GableSouth, WEST)?;
            Vec::new()
        }
        Part::Chimney => {
            chimney_into(&mut b, roof)?;
            return Ok(b.finish(&format!("{} ({})", p.name(), roof.key()), 2, 2, &[]));
        }
        Part::Side | Part::SideDoor => {
            wall_into(&mut b, if p == Part::SideDoor { WallKind::Door } else { WallKind::Window(Ends::None) }, roof, SOUTH)?;
            roof_into(&mut b, roof, ROOF_SOUTH)?;
            edge.to_vec()
        }
        Part::CornerWest | Part::CornerEast => {
            let west_side = p == Part::CornerWest;
            wall_into(&mut b, WallKind::Logs(if west_side { Ends::Left } else { Ends::Right }), roof, SOUTH)?;
            roof_into(&mut b, roof, ROOF_SOUTH)?;
            // The end wall, built on the south edge and turned onto the west edge (270°) or the
            // east edge (90°); seen from outside, its corner is on the right (west) or left (east).
            let mut end = Builder::new(page);
            wall_into(&mut end, WallKind::Logs(if west_side { Ends::Right } else { Ends::Left }), roof, if west_side { WEST } else { EAST })?;
            merge(&mut b, end, if west_side { 3 } else { 1 });
            // The gable rises from the south eave to the ridge on the north edge.
            if west_side {
                gable_into(&mut b, roof, true, WEST)?;
            } else {
                let mut g = Builder::new(page);
                gable_into(&mut g, roof, false, EAST)?;
                merge(&mut b, g, 2);
            }
            [edge, if west_side { west } else { east }].concat()
        }
    };
    Ok(b.finish(&format!("{} ({})", p.name(), roof.key()), 4, 4, &blocked))
}

/// Adds `other`'s faces to `b`, turned `rot` quarter turns about the 4 × 4 tile.
fn merge(b: &mut Builder, other: Builder, rot: u8) {
    let turn = |v: [i16; 3]| -> [i16; 3] {
        let (x, z) = (v[0], v[2]);
        let (x, z) = match rot % 4 {
            0 => (x, z),
            1 => (M - z, x),
            2 => (M - x, M - z),
            _ => (z, M - x),
        };
        [x, v[1], z]
    };
    let mut remap: HashMap<u16, u16> = HashMap::new();
    for (k, t) in other.tiles.iter().enumerate() {
        let key = other.tile_of.iter().find(|(_, &v)| v as usize == k).map(|(key, _)| *key);
        let at = match key.and_then(|key| b.tile_of.get(&key).copied()) {
            Some(existing) => existing,
            None => {
                b.tiles.push(t.clone());
                let n = (b.tiles.len() - 1) as u16;
                if let Some(key) = key {
                    b.tile_of.insert(key, n);
                }
                n
            }
        };
        remap.insert(k as u16, at);
    }
    for pp in other.prims {
        let mut q = pp.prim;
        for i in 0..q.n as usize {
            let v = other.pool[q.idx[i] as usize];
            // Pools are stored Y down; `vertex` flips again.
            q.idx[i] = b.vertex(turn([v[0], -v[1], v[2]]));
        }
        b.prims.push(PrefabPrim { prim: q, tile: pp.tile.map(|t| remap[&t]) });
    }
}

/// A log house `length` modules long (west to east) and two deep, its door
/// in module `door` (from the west) on the south side, windows in the other
/// side modules, a chimney over module `chimney` on the north slope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HouseSpec {
    pub roof: Roof,
    pub length: u8,
    pub door: Option<u8>,
    pub windows: bool,
    pub chimney: Option<u8>,
}

pub const MAX_LENGTH: u8 = 8;

/// The whole house as one prefab: `4·length × 8` cells, every cell closed.
pub fn house(spec: HouseSpec, page: &Texture) -> Result<Prefab> {
    let n = spec.length;
    if !(1..=MAX_LENGTH).contains(&n) {
        return Err(EditError::new("HOUSE_SIZE", format!("A house is 1 to {MAX_LENGTH} modules long.")));
    }
    if spec.door.is_some_and(|d| d >= n) || spec.chimney.is_some_and(|c| c >= n) {
        return Err(EditError::new("HOUSE_MODULE", format!("Doors and chimneys go in modules 0 to {}.", n - 1)));
    }
    let roof = spec.roof;
    let mut pieces: Vec<(Prefab, u8, u8, u8)> = Vec::new();
    let mut piece = |f: &dyn Fn(&mut Builder) -> Result<()>, col: u8, row: u8, rot: u8| -> Result<()> {
        let mut b = Builder::new(page);
        f(&mut b)?;
        pieces.push((b.finish("piece", 4, 4, &[]), col, row, rot));
        Ok(())
    };
    let ends_of = |i: u8, left_is_west: bool| -> Ends {
        let (first, last) = (i == 0, i == n - 1);
        match (first, last) {
            (true, true) => Ends::Both,
            (true, false) => if left_is_west { Ends::Left } else { Ends::Right },
            (false, true) => if left_is_west { Ends::Right } else { Ends::Left },
            _ => Ends::None,
        }
    };
    for i in 0..n {
        let col = i * 4;
        let side = |ends: Ends, door: bool| {
            if door {
                WallKind::Door
            } else if spec.windows && ends != Ends::Both {
                WallKind::Window(ends)
            } else {
                WallKind::Logs(ends)
            }
        };
        // South side: seen from outside, west is on the left; north side: east is on the left.
        let south = side(ends_of(i, true), spec.door == Some(i));
        let north = side(ends_of(i, false), false);
        piece(&|b| wall_into(b, south, roof, SOUTH), col, 0, 0)?;
        piece(&|b| roof_into(b, roof, ROOF_SOUTH), col, 0, 0)?;
        piece(&|b| wall_into(b, north, roof, NORTH), col, 4, 2)?;
        piece(&|b| roof_into(b, roof, ROOF_NORTH), col, 4, 2)?;
    }
    // Gable ends: seen from the west, north is on the left; from the east, south is.
    let last = (n - 1) * 4;
    piece(&|b| wall_into(b, WallKind::Logs(Ends::Right), roof, WEST), 0, 0, 3)?;
    piece(&|b| wall_into(b, WallKind::Logs(Ends::Left), roof, WEST), 0, 4, 3)?;
    piece(&|b| gable_into(b, roof, true, WEST), 0, 0, 0)?;
    piece(&|b| gable_into(b, roof, false, WEST), 0, 4, 0)?;
    piece(&|b| wall_into(b, WallKind::Logs(Ends::Left), roof, EAST), last, 0, 1)?;
    piece(&|b| wall_into(b, WallKind::Logs(Ends::Right), roof, EAST), last, 4, 1)?;
    piece(&|b| gable_into(b, roof, false, EAST), last, 0, 2)?;
    piece(&|b| gable_into(b, roof, true, EAST), last, 4, 2)?;
    let mut parts: Vec<prefab::Part> = pieces.iter().map(|(p, col, row, rot)| prefab::Part { prefab: p, col: *col, row: *row, rot: *rot, lift: 0 }).collect();
    let chimney = match spec.chimney {
        Some(i) => {
            let mut b = Builder::new(page);
            chimney_into(&mut b, roof)?;
            Some((b.finish("chimney", 2, 2, &[]), i * 4 + 1))
        }
        None => None,
    };
    if let Some((p, col)) = &chimney {
        parts.push(prefab::Part { prefab: p, col: *col, row: 4, rot: 0, lift: 0 });
    }
    let name = format!("Log house {n}×2 ({})", roof.key());
    let mut out = prefab::compose(&parts, &name)?;
    out.heights.iter_mut().for_each(|h| *h = PatchCell::Blocked);
    out.category = "Buildings".to_string();
    out.source_chunk = SOURCE_CHUNK;
    Ok(out)
}

/// Chunk 78's texture page, for [`part`] and [`house`].
pub fn page_of(chunk78: &Chunk) -> &Texture {
    &chunk78.texture
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tim::VramRect;

    /// A page with every palette the parts use (CLUT rows 480–495 at x 96) and some texels.
    fn page() -> Texture {
        let mut c = Chunk::blank(0);
        c.texture.clut = Some((VramRect { x: 96, y: 480, w: 16, h: 16 }, (0..256).map(|i| i as u16 | 0x8000).collect()));
        c.texture.pixels.iter_mut().enumerate().for_each(|(i, b)| *b = (i % 251) as u8);
        c.texture
    }

    #[test]
    fn every_part_is_a_tile_with_its_piece_on_an_edge() {
        let t = page();
        for roof in Roof::ALL {
            for p in Part::ALL {
                let f = part(p, roof, &t).unwrap();
                let (w, h) = if p == Part::Chimney { (2, 2) } else { (4, 4) };
                assert_eq!((f.area.w(), f.area.h()), (w, h), "{p:?}");
                // Inside the tile, at most 256 up.
                for v in &f.pool {
                    assert!((0..=w as i16 * 32).contains(&v[0]) && (0..=h as i16 * 32).contains(&v[2]) && (-256..=0).contains(&v[1]), "{p:?} {v:?}");
                }
                // Walls close the row they stand on, and only that.
                let blocked = f.heights.iter().filter(|c| **c == PatchCell::Blocked).count();
                let walls = matches!(p, Part::Wall | Part::WallWindow | Part::WallDoor | Part::WallEndLeft | Part::WallEndRight | Part::Side | Part::SideDoor);
                let corners = matches!(p, Part::CornerWest | Part::CornerEast);
                assert_eq!(blocked, if walls { 4 } else if corners { 7 } else { 0 }, "{p:?}");
            }
        }
    }

    #[test]
    fn a_turned_end_wall_stands_on_the_tiles_west_edge() {
        let f = part(Part::CornerWest, Roof::Blue, &page()).unwrap();
        // The west wall's faces lie in the plane x = 0, from the ground to the trim.
        let west: Vec<_> = f.prims.iter().filter(|p| p.prim.idx[..p.prim.n as usize].iter().all(|&i| f.pool[i as usize][0] == 0)).collect();
        assert_eq!(west.len(), 4, "two log rows, the trim and the gable");
        assert!(f.pool.iter().any(|v| v[0] == 0 && v[2] == 128 && v[1] == -RIDGE), "the gable reaches the ridge on the north edge");
    }

    #[test]
    fn houses_of_every_length_put_together_and_place() {
        let t = page();
        for length in 1..=MAX_LENGTH {
            let spec = HouseSpec { roof: Roof::Red, length, door: Some(length / 2), windows: true, chimney: Some(0) };
            let h = house(spec, &t).unwrap();
            assert_eq!((h.area.w(), h.area.h()), (4 * length as usize, 8));
            assert!(h.heights.iter().all(|c| *c == PatchCell::Blocked));
            assert_eq!(h.height(), CHIMNEY_TOP);
            // Shared tiles: one copy of each texture however long the house is.
            assert!(h.tiles.len() <= 12, "{length}: {} tiles", h.tiles.len());
            let mut area = Chunk::new_area(1328);
            prefab::place(&mut area, &h, prefab::Placement { col: 10, row: 10, rot: 1, ground_y: None, write_ground: false, lift: 0 }).unwrap();
            assert!(area.serialize().is_ok());
        }
        assert_eq!(house(HouseSpec { roof: Roof::Red, length: 9, door: None, windows: false, chimney: None }, &t).unwrap_err().code, "HOUSE_SIZE");
        assert_eq!(house(HouseSpec { roof: Roof::Red, length: 2, door: Some(2), windows: false, chimney: None }, &t).unwrap_err().code, "HOUSE_MODULE");
    }
}
