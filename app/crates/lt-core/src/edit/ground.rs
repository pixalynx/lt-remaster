//! Ground painting for new areas (screen 10): a 32 × 32 map of 128-unit
//! tiles, each a material, a height level and an optional ramp, built into
//! the chunk's terrain the way retail chunks build theirs.
//!
//! Retail ground (chunk 78, `examples/ground_survey.rs`) is 128-unit quads
//! textured with 32 × 32-texel tiles of the shared page 0x08, cliffs are
//! 128-wide rock quads 64 units high, and ramps rise 64 over a tile. Every
//! chunk outside `ChunkTextureSetIds` loads region set 0 with the field
//! atlas, new areas and the Knight's village included, so a new area samples
//! the village's ground tiles where they are: painting costs no texture
//! space (`docs/research/05-props.md` §7).
//!
//! The tiles are hand-painted with a few transition pieces, recoloured by
//! palette: CLUT 0x7800 is grass and dirt, 0x78C0 grass and sand, 0x7900
//! beach and water. Dirt, sand and water tiles next to grass take the piece
//! for their side, outer corner or inner corner, turned into place, so the
//! edge is drawn on their side of the border. Walk heights follow the
//! surface (water blocks walking); the outermost ring of cells stays a wall,
//! as in a fresh new area.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::world::Prim;

use super::chunk::{Chunk, Group, CELLS};
use super::{EditError, Result};

/// Tiles per side (128 units each).
pub const TILES: usize = 32;
/// Units per tile and per height level.
pub const TILE: i16 = 128;
pub const STEP: i16 = 64;
/// The ground of a fresh new area (PSX Y, down).
pub const BASE_Y: i16 = -128;
/// Levels a tile can take: one below the base, seven above.
pub const MIN_LEVEL: i8 = -1;
pub const MAX_LEVEL: i8 = 7;

/// Shared page 0x08 (VRAM x 512, y 0), 4bpp.
pub(crate) const PAGE: u16 = 0x08;
pub(crate) const GRASS_DIRT: u16 = 0x7800;
pub(crate) const GRASS_SAND: u16 = 0x78C0;
pub(crate) const BEACH_WATER: u16 = 0x7900;
/// The colour of a fresh new area's ground.
const PLAIN_RGB: [u8; 3] = [84, 116, 70];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Material {
    /// The untextured ground of a fresh new area.
    Plain = 0,
    Grass = 1,
    Forest = 2,
    Dirt = 3,
    Sand = 4,
    Water = 5,
    Rubble = 6,
    Mossy = 7,
}

impl Material {
    pub const ALL: [Material; 8] = [
        Material::Plain,
        Material::Grass,
        Material::Forest,
        Material::Dirt,
        Material::Sand,
        Material::Water,
        Material::Rubble,
        Material::Mossy,
    ];

    pub fn from_id(id: u8) -> Option<Material> {
        Material::ALL.get(id as usize).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Material::Plain => "Plain",
            Material::Grass => "Grass",
            Material::Forest => "Forest floor",
            Material::Dirt => "Dirt",
            Material::Sand => "Sand",
            Material::Water => "Water",
            Material::Rubble => "Rubble",
            Material::Mossy => "Mossy stones",
        }
    }

    /// The plain tile (column, row of 32-texel tiles on page 0x08) and its
    /// CLUT; `None` for the untextured ground.
    pub fn tile(self) -> Option<([u8; 2], u16)> {
        Some(match self {
            Material::Plain => return None,
            Material::Grass => ([0, 0], GRASS_DIRT),
            Material::Forest => ([0, 2], GRASS_DIRT),
            Material::Dirt => ([0, 1], GRASS_DIRT),
            Material::Sand => ([0, 1], GRASS_SAND),
            Material::Water => ([0, 1], BEACH_WATER),
            Material::Rubble => ([2, 3], GRASS_DIRT),
            Material::Mossy => ([7, 3], GRASS_DIRT),
        })
    }

    /// The palette of its edge pieces and which neighbours it draws an edge against.
    pub(crate) fn edges(self) -> Option<(u16, fn(Material) -> bool)> {
        let grassy = |m: Material| matches!(m, Material::Grass | Material::Plain);
        let dry = |m: Material| m != Material::Water;
        match self {
            Material::Dirt => Some((GRASS_DIRT, grassy)),
            Material::Sand => Some((GRASS_SAND, grassy)),
            Material::Water => Some((BEACH_WATER, dry)),
            _ => None,
        }
    }

    /// Grass grows over the lip of a cliff below it.
    pub(crate) fn grassy_lip(self) -> bool {
        matches!(self, Material::Grass | Material::Forest | Material::Plain)
    }
}

/// Edge pieces (column, row on page 0x08), drawn unturned with the other
/// material on the west side, on the west and south sides, and in the
/// north-west corner.
const SIDE: [u8; 2] = [6, 2];
const OUTER: [u8; 2] = [6, 3];
const INNER: [u8; 2] = [7, 2];
/// Grass tiles with the other material along their south side (two
/// drawings, alternated so a long border doesn't repeat).
const GRASS_SIDE: [[u8; 2]; 2] = [[0, 3], [1, 5]];
/// Cliff bands (texels on page 0x08, 32 × 16): rock with a grass lip, plain rock.
const CLIFF_LIP: [u8; 2] = [64, 0];
const CLIFF_ROCK: [u8; 2] = [32, 80];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundTile {
    pub material: u8,
    pub level: i8,
    /// 0 flat; 1–4 rises one level toward the north, east, south, west.
    #[serde(default)]
    pub ramp: u8,
}

/// The map, `TILES × TILES`, row-major from the south-west corner (rows go north).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundMap {
    pub tiles: Vec<GroundTile>,
}

impl Default for GroundMap {
    fn default() -> Self {
        GroundMap { tiles: vec![GroundTile::default(); TILES * TILES] }
    }
}

impl GroundMap {
    pub fn get(&self, col: i32, row: i32) -> Option<GroundTile> {
        ((0..TILES as i32).contains(&col) && (0..TILES as i32).contains(&row)).then(|| self.tiles[row as usize * TILES + col as usize])
    }

    pub fn check(&self) -> Result<()> {
        if self.tiles.len() != TILES * TILES {
            return Err(EditError::new("GROUND_SIZE", format!("A ground map has {} tiles; this one has {}.", TILES * TILES, self.tiles.len())));
        }
        for t in &self.tiles {
            if Material::from_id(t.material).is_none() {
                return Err(EditError::new("GROUND_MATERIAL", format!("There is no ground material {}.", t.material)));
            }
            if !(MIN_LEVEL..=MAX_LEVEL).contains(&t.level) || (t.ramp > 0 && t.level >= MAX_LEVEL) || t.ramp > 4 {
                return Err(EditError::new("GROUND_LEVEL", format!("Levels go from {MIN_LEVEL} to {MAX_LEVEL} (a ramp rises one more).")));
            }
        }
        Ok(())
    }

    /// Surface y (PSX, down) of a tile's corners: north-west, north-east, south-east, south-west.
    pub(crate) fn corners(t: GroundTile) -> [i16; 4] {
        let y = BASE_Y - t.level as i16 * STEP;
        let up = y - STEP;
        match t.ramp {
            1 => [up, up, y, y],
            2 => [y, up, up, y],
            3 => [y, y, up, up],
            4 => [up, y, y, up],
            _ => [y; 4],
        }
    }
}

/// Whether `c` is a fresh new area's ground (untextured flat quads at the
/// base height and nothing else): what the painter replaces.
pub fn is_blank(c: &Chunk) -> bool {
    c.groups.iter().flat_map(|g| g.prims.iter()).all(|p| !p.textured() && p.idx[..p.n as usize].iter().all(|&i| c.pool[i as usize][1] == BASE_Y))
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundReport {
    pub tiles: u32,
    pub cliffs: u32,
    pub polygons: u32,
}

/// Builds `map` into `c`, a fresh new area's ground (see [`is_blank`]): its
/// terrain is replaced, other polygons stay, and every walk cell follows the
/// new surface.
pub fn apply(c: &mut Chunk, map: &GroundMap) -> Result<GroundReport> {
    map.check()?;
    // Keep what isn't ground, then rebuild the pool from what is left.
    let keep: Vec<Group> = c
        .groups
        .iter()
        .map(|g| Group { centre: g.centre, prims: g.prims.iter().filter(|p| !super::props::is_terrain(c, p)).copied().collect() })
        .filter(|g| !g.prims.is_empty())
        .collect();
    let old = std::mem::take(&mut c.pool);
    let mut b = Mesh::default();
    c.groups = keep
        .into_iter()
        .map(|mut g| {
            for p in &mut g.prims {
                for k in 0..p.n as usize {
                    p.idx[k] = b.vertex(old[p.idx[k] as usize]);
                }
            }
            g
        })
        .collect();
    let first = c.groups.len();
    let mut report = GroundReport::default();
    let look = looks(map);
    let at = |i: usize| -2048 + i as i16 * TILE;
    for row in 0..TILES {
        for col in 0..TILES {
            let t = map.tiles[row * TILES + col];
            let m = Material::from_id(t.material).unwrap();
            let h = GroundMap::corners(t);
            let (x0, z0) = (at(col), at(row));
            // North-west, north-east, south-east, south-west.
            let corner = [[x0, h[0], z0 + TILE], [x0 + TILE, h[1], z0 + TILE], [x0 + TILE, h[2], z0], [x0, h[3], z0]];
            match look[row * TILES + col] {
                None => b.flat(corner, PLAIN_RGB),
                Some((tile, clut, turn, _)) => b.tile(corner, tile, clut, turn),
            }
            if m != Material::Plain {
                report.tiles += 1;
            }
            // Cliffs on the sides where this tile stands above its neighbour (or the chunk's edge).
            for side in 0..4 {
                let (dc, dr) = [(0, 1), (1, 0), (0, -1), (-1, 0)][side];
                // This tile's edge corners on that side, left to right as seen from outside.
                let (a, bb) = [(1, 0), (2, 1), (3, 2), (0, 3)][side];
                let (top_a, top_b) = (h[a], h[bb]);
                let (low_a, low_b) = match map.get(col as i32 + dc, row as i32 + dr) {
                    Some(n) => {
                        let nh = GroundMap::corners(n);
                        // The neighbour's corners on the shared edge.
                        let (na, nb) = [(2, 3), (3, 0), (0, 1), (1, 2)][side];
                        (nh[na], nh[nb])
                    }
                    // The chunk's edge: down to the base ground.
                    None => (BASE_Y, BASE_Y),
                };
                if top_a < low_a || top_b < low_b {
                    report.cliffs += b.cliff([corner[a], corner[bb]], [low_a.max(top_a), low_b.max(top_b)], m.grassy_lip(), side);
                }
            }
        }
    }
    report.polygons = b.prims.len() as u32;
    // Draw groups of 512 units, like the retail ground.
    let mut groups: Vec<((i32, i32), Group)> = Vec::new();
    for p in b.prims {
        let n = p.n as usize;
        let cx = p.idx[..n].iter().map(|&i| b.pool[i as usize][0] as i32).sum::<i32>() / n as i32;
        let cz = p.idx[..n].iter().map(|&i| b.pool[i as usize][2] as i32).sum::<i32>() / n as i32;
        let key = ((cx + 2048).div_euclid(512), (cz + 2048).div_euclid(512));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.prims.push(p),
            None => groups.push((key, Group { centre: [0; 3], prims: vec![p] })),
        }
    }
    c.pool = b.pool;
    c.groups.extend(groups.into_iter().map(|(_, g)| g));
    for g in first..c.groups.len() {
        c.recompute_centre(g);
    }
    // Walk heights: the surface under each cell centre − 1; water blocks; the outer ring is a wall.
    for i in 0..CELLS {
        let (col, row) = (i % 128, i / 128);
        c.heights[i] = if col == 0 || row == 0 || col == 127 || row == 127 {
            0
        } else {
            let t = map.tiles[(row / 4) * TILES + col / 4];
            if t.material == Material::Water as u8 {
                -1
            } else {
                let h = GroundMap::corners(t);
                // Bilinear over the tile: fx east, fz north.
                let (fx, fz) = (((col % 4) as f32 + 0.5) / 4.0, ((row % 4) as f32 + 0.5) / 4.0);
                let south = h[3] as f32 * (1.0 - fx) + h[2] as f32 * fx;
                let north = h[0] as f32 * (1.0 - fx) + h[1] as f32 * fx;
                (south * (1.0 - fz) + north * fz).round() as i16 - 1
            }
        };
    }
    c.sizes()?;
    Ok(report)
}

/// What a tile draws: its plain tile or an edge piece (column, row on page
/// 0x08), the CLUT, the quarter turns clockwise, and the sides (north, east,
/// south, west) whose border the piece draws. `None` draws the untextured
/// ground.
pub(crate) type Look = Option<([u8; 2], u16, u8, [bool; 4])>;

/// The material of the tile at (column, row) in some frame (tiles past the map
/// included, for the overworld's land), `None` where nothing is painted.
pub(crate) type MaterialAt<'a> = &'a dyn Fn(i32, i32) -> Option<Material>;

const N: usize = 0;
const E: usize = 1;
const S: usize = 2;
const W: usize = 3;
/// Neighbour offsets (column, row) toward north, east, south, west.
pub(crate) const TOWARD: [(i32, i32); 4] = [(0, 1), (1, 0), (0, -1), (-1, 0)];

/// Every tile's look ([`look_with`] over the map).
fn looks(map: &GroundMap) -> Vec<Look> {
    let material = |col: i32, row: i32| map.get(col, row).and_then(|t| Material::from_id(t.material));
    (0..TILES * TILES).map(|i| look_with(&material, (i % TILES) as i32, (i / TILES) as i32)).collect()
}

/// A tile's look. Dirt, sand and water tiles next to grass take the edge
/// piece for their side, outer corner or inner corner; where one borders
/// grass on a side its piece doesn't draw (opposite sides, three sides), the
/// grass tile there draws the border instead with a grass-side piece.
pub(crate) fn look_with(material: MaterialAt, col: i32, row: i32) -> Look {
    let own = edged_with(material, col, row);
    if material(col, row) != Some(Material::Grass) {
        return own;
    }
    for (s, (dc, dr)) in TOWARD.into_iter().enumerate() {
        let Some(m) = material(col + dc, row + dr) else { continue };
        // Dirt and sand only: a grass tile with a water strip would turn into beach.
        if !matches!(m, Material::Dirt | Material::Sand) {
            continue;
        }
        let drawn = edged_with(material, col + dc, row + dr).is_some_and(|l| l.3[(s + 2) % 4]);
        if !drawn {
            let (_, clut) = m.tile().unwrap();
            // Unturned the other material is south.
            let turn = [2u8, 3, 0, 1][s];
            let mut sides = [false; 4];
            sides[s] = true;
            let piece = GRASS_SIDE[((col + row) & 1) as usize];
            return Some((piece, clut, turn, sides));
        }
    }
    own
}

/// The page and size of the ground's texture tiles, for decals that reuse them.
pub(super) const DECAL_PAGE: u16 = PAGE;

/// How patches of `material` at tiles `(col, row)` (0–29 each), on grass,
/// draw as decals: each patch tile and each grass tile that draws a border
/// for one, as (col, row, texture tile on page 0x08, CLUT, quarter turns
/// clockwise). Grass tiles outside `inside` aren't returned.
pub(super) fn patch_looks(patches: &[(i32, i32, Material)], inside: &dyn Fn(i32, i32) -> bool) -> Vec<(i32, i32, [u8; 2], u16, u8)> {
    // Off the map's edge by a tile, so every patch has neighbours.
    let mut map = GroundMap { tiles: vec![GroundTile { material: Material::Grass as u8, level: 0, ramp: 0 }; TILES * TILES] };
    for &(c, r, m) in patches {
        if let Some(t) = map.tiles.get_mut((r + 1) as usize * TILES + (c + 1) as usize) {
            t.material = m as u8;
        }
    }
    let look = looks(&map);
    let mut out = Vec::new();
    for row in 0..TILES as i32 {
        for col in 0..TILES as i32 {
            let (c, r) = (col - 1, row - 1);
            let is_patch = patches.iter().any(|p| p.0 == c && p.1 == r);
            let Some((tile, clut, turn, sides)) = look[(row * TILES as i32 + col) as usize] else { continue };
            // A grass tile draws only where it carries a border, and only inside.
            if is_patch || (sides.iter().any(|&s| s) && inside(c, r)) {
                out.push((c, r, tile, clut, turn));
            }
        }
    }
    out
}

/// A tile's own look (before grass tiles take the borders it leaves).
fn edged_with(material: MaterialAt, col: i32, row: i32) -> Look {
    let m = material(col, row)?;
    let (tile, clut) = m.tile()?;
    let Some((edge_clut, other)) = m.edges() else { return Some((tile, clut, 0, [false; 4])) };
    let is_other = |dc: i32, dr: i32| material(col + dc, row + dr).is_some_and(other);
    let side = TOWARD.map(|(dc, dr)| is_other(dc, dr));
    let only = |a: usize, b: Option<usize>| {
        let mut s = [false; 4];
        s[a] = true;
        if let Some(b) = b {
            s[b] = true;
        }
        s
    };
    // Outer corners: unturned the other material is west and south.
    for (turn, (a, b)) in [(W, S), (N, W), (E, N), (S, E)].into_iter().enumerate() {
        if side[a] && side[b] {
            return Some((OUTER, edge_clut, turn as u8, only(a, Some(b))));
        }
    }
    // Sides: unturned the other material is west.
    for (turn, s) in [W, N, E, S].into_iter().enumerate() {
        if side[s] {
            return Some((SIDE, edge_clut, turn as u8, only(s, None)));
        }
    }
    // Inner corners: unturned the other material is north-west.
    for (turn, (dc, dr)) in [(-1, 1), (1, 1), (1, -1), (-1, -1)].into_iter().enumerate() {
        if is_other(dc, dr) {
            return Some((INNER, edge_clut, turn as u8, [false; 4]));
        }
    }
    Some((tile, clut, 0, [false; 4]))
}

/// New polygons over a deduplicated pool.
#[derive(Default)]
pub(crate) struct Mesh {
    pub(crate) pool: Vec<[i16; 3]>,
    index: HashMap<[i16; 3], u16>,
    pub(crate) prims: Vec<Prim>,
}

impl Mesh {
    pub(crate) fn vertex(&mut self, v: [i16; 3]) -> u16 {
        *self.index.entry(v).or_insert_with(|| {
            self.pool.push(v);
            (self.pool.len() - 1) as u16
        })
    }

    /// A quad from corners north-west, north-east, south-east, south-west, in
    /// PSX strip order (top-left, top-right, bottom-left, bottom-right as seen from above).
    fn quad_idx(&mut self, c: [[i16; 3]; 4]) -> [u16; 4] {
        [self.vertex(c[0]), self.vertex(c[1]), self.vertex(c[3]), self.vertex(c[2])]
    }

    pub(crate) fn flat(&mut self, c: [[i16; 3]; 4], rgb: [u8; 3]) {
        let idx = self.quad_idx(c);
        self.prims.push(Prim { list: 2, code: 0x28, n: 4, idx, rgb: [rgb; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 });
    }

    /// A ground tile, its texture turned `turn` quarter turns clockwise (seen from above).
    pub(crate) fn tile(&mut self, c: [[i16; 3]; 4], tile: [u8; 2], clut: u16, turn: u8) {
        let idx = self.quad_idx(c);
        let (u0, v0) = (tile[0] * 32, tile[1] * 32);
        // Texture corners clockwise from top-left; corner i of the quad takes texture corner i − turn.
        let tex = [[u0, v0], [u0 + 31, v0], [u0 + 31, v0 + 31], [u0, v0 + 31]];
        let at = |i: usize| tex[(i + 4 - turn as usize % 4) % 4];
        // Quad order: north-west, north-east, south-west, south-east.
        let uv = [at(0), at(1), at(3), at(2)];
        self.prims.push(Prim { list: 3, code: 0x2C, n: 4, idx, rgb: [[128; 3]; 4], uv, clut, tpage: PAGE });
    }

    /// Rock from a tile's edge (`top`, left to right as seen from outside)
    /// down to `low` (the neighbour's heights there), in 64-unit bands, the
    /// top one with a grass lip. Returns the polygons added.
    pub(crate) fn cliff(&mut self, top: [[i16; 3]; 2], low: [i16; 2], lip: bool, side: usize) -> u32 {
        // Light by facing: north, east, south, west.
        let shade = [[70; 3], [84; 3], [112; 3], [100; 3]][side];
        let mut added = 0;
        let mut upper = [top[0][1], top[1][1]];
        let mut band = 0;
        while upper[0] < low[0] || upper[1] < low[1] {
            let lower = [(upper[0] + STEP).min(low[0]), (upper[1] + STEP).min(low[1])];
            let a_top = [top[0][0], upper[0], top[0][2]];
            let b_top = [top[1][0], upper[1], top[1][2]];
            let a_low = [top[0][0], lower[0], top[0][2]];
            let b_low = [top[1][0], lower[1], top[1][2]];
            let tex = if band == 0 && lip { CLIFF_LIP } else { CLIFF_ROCK };
            // Texel rows for this band's depth at each end (4 units per texel, 16 rows a band).
            let rows = |d: i16| (d / 4).clamp(0, 15) as u8;
            let (ra, rb) = (rows(lower[0] - upper[0]), rows(lower[1] - upper[1]));
            let uv = [[tex[0], tex[1]], [tex[0] + 31, tex[1]], [tex[0], tex[1] + ra], [tex[0] + 31, tex[1] + rb]];
            let idx = [self.vertex(a_top), self.vertex(b_top), self.vertex(a_low), self.vertex(b_low)];
            let (list, code, n, idx) = if a_top == a_low {
                // A triangle where one end has no height (beside a ramp).
                (5u8, 0x34u8, 3u8, [idx[0], idx[1], idx[3], 0])
            } else if b_top == b_low {
                (5, 0x34, 3, [idx[0], idx[1], idx[2], 0])
            } else {
                (7, 0x3C, 4, idx)
            };
            let uv = if n == 3 { if a_top == a_low { [uv[0], uv[1], uv[3], [0; 2]] } else { [uv[0], uv[1], uv[2], [0; 2]] } } else { uv };
            self.prims.push(Prim { list, code, n, idx, rgb: [shade; 4], uv, clut: GRASS_DIRT, tpage: PAGE });
            added += 1;
            upper = lower;
            band += 1;
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(f: impl Fn(usize, usize) -> GroundTile) -> GroundMap {
        GroundMap { tiles: (0..TILES * TILES).map(|i| f(i % TILES, i / TILES)).collect() }
    }

    #[test]
    fn a_blank_map_is_the_new_areas_ground_again() {
        let mut c = Chunk::new_area(1328);
        assert!(is_blank(&c));
        let r = apply(&mut c, &GroundMap::default()).unwrap();
        assert_eq!((r.tiles, r.cliffs, r.polygons), (0, 0, 1024));
        assert!(is_blank(&c));
        assert_eq!((c.heights[64 * 128 + 64], c.heights[0]), (-129, 0));
        assert!(c.serialize().is_ok());
    }

    #[test]
    fn dirt_takes_its_edges_and_a_plateau_its_cliffs() {
        let mut c = Chunk::new_area(1328);
        // A dirt path along row 10, grass around it; a plateau two levels up at 20..24.
        let map = map_with(|col, row| {
            let level = if (20..24).contains(&col) && (20..24).contains(&row) { 2 } else { 0 };
            let material = if row == 10 { Material::Dirt } else { Material::Grass } as u8;
            GroundTile { material, level, ramp: 0 }
        });
        let r = apply(&mut c, &map).unwrap();
        assert_eq!(r.tiles, 1024);
        // 4 × 4 tiles, 16 edge tiles each 2 bands: 32 cliff quads.
        assert_eq!(r.cliffs, 32);
        // The path's tiles have grass north and south: side pieces.
        let path: Vec<&Prim> = c.groups.iter().flat_map(|g| g.prims.iter()).filter(|p| {
            let z = c.pool[p.idx[0] as usize][2];
            p.list == 3 && z == -2048 + 11 * 128 && c.pool[p.idx[2] as usize][2] == -2048 + 10 * 128
        }).collect();
        assert!(path.iter().all(|p| p.uv.iter().map(|u| u[0] / 32).min() == Some(SIDE[0]) && p.clut == GRASS_DIRT), "{:?}", path.first());
        // Walk heights on the plateau are two levels up.
        assert_eq!(c.heights[(21 * 4) * 128 + 21 * 4], BASE_Y - 2 * STEP - 1);
        assert!(c.serialize().is_ok());
    }

    #[test]
    fn a_ramp_leads_up_a_level_and_water_blocks() {
        let mut c = Chunk::new_area(1328);
        let map = map_with(|col, row| match (col, row) {
            (5, 5) => GroundTile { material: Material::Grass as u8, level: 0, ramp: 2 },
            (6, 5) => GroundTile { material: Material::Grass as u8, level: 1, ramp: 0 },
            (8, 8) => GroundTile { material: Material::Water as u8, level: -1, ramp: 0 },
            _ => GroundTile { material: Material::Grass as u8, level: 0, ramp: 0 },
        });
        apply(&mut c, &map).unwrap();
        // Across the ramp the walk height climbs from the base toward the next level.
        let (west, east) = (c.heights[(5 * 4 + 2) * 128 + 5 * 4], c.heights[(5 * 4 + 2) * 128 + 5 * 4 + 3]);
        assert!(west > east && east > BASE_Y - STEP - 1 && west < BASE_Y, "{west} {east}");
        assert_eq!(c.heights[(8 * 4 + 1) * 128 + 8 * 4 + 1], -1);
        assert!(c.serialize().is_ok());
        let bad = map_with(|_, _| GroundTile { material: 9, level: 0, ramp: 0 });
        assert_eq!(apply(&mut c, &bad).unwrap_err().code, "GROUND_MATERIAL");
    }
}

/// The materials' plain tiles side by side (32 × 32 px each, in
/// [`Material::ALL`] order) from a new area's field VRAM: the painter's
/// swatches. PNG bytes.
pub fn swatches(vram: &crate::vram::Vram) -> Option<Vec<u8>> {
    let n = Material::ALL.len();
    let w = 32 * n;
    let mut rgba = vec![0u8; w * 32 * 4];
    for (k, m) in Material::ALL.into_iter().enumerate() {
        for y in 0..32 {
            for x in 0..32 {
                let rgb = match m.tile() {
                    None => PLAIN_RGB,
                    Some((tile, clut)) => {
                        let (u, v) = (tile[0] as usize * 32 + x, tile[1] as usize * 32 + y);
                        let hw = vram.get(512 + u / 4, v);
                        let i = (hw >> ((u & 3) * 4)) & 0xF;
                        let c = vram.get((clut as usize & 0x3F) * 16 + i as usize, clut as usize >> 6);
                        [crate::vram::c5(c), crate::vram::c5(c >> 5), crate::vram::c5(c >> 10)]
                    }
                };
                let o = (y * w + k * 32 + x) * 4;
                rgba[o..o + 3].copy_from_slice(&rgb);
                rgba[o + 3] = 255;
            }
        }
    }
    super::obj::encode_png(w as u32, 32, &rgba)
}
