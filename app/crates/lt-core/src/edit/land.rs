//! New land on the overworld (screen 10's Land brush and island builder):
//! ground painted over a grid chunk's sea, in the Ground tool's tiles.
//!
//! Open sea is built the way [`super::ground`] builds a new area: a 32 × 32
//! grid of 128-unit FT4 quads on the shared page 0x08 (CLUT 0x780A), every
//! vertex at y 0, walk heights −1 (156 grid chunks all sea, and the sea tiles of
//! 101 coastal ones). A [`LandMap`] paints tiles of it: each painted tile's
//! sea quad goes and a ground tile takes its place, with a material, a level
//! (64 units, level 0 at y −128 like the retail lowlands) and an optional
//! ramp. Unpainted tiles stay as the chunk has them.
//!
//! Rock cliffs fill every side where a painted tile stands above what is next
//! to it: another painted tile, the chunk's own ground (its top surface at
//! the shared corners), the sea (y 0), or, past the chunk's edge, whatever
//! the caller says the neighbouring chunk has there. The higher side builds
//! the face, so two chunks never both draw one. Edge vertices sit where the
//! retail edges do (east +2051, south −2051, three units over the
//! neighbour), so a seam between land tiles closes like a retail one.
//!
//! Walk heights follow the new surface (water blocks); the land's cells take
//! the map's zone (layer B), so they get land encounters and music instead
//! of the sea's.
//!
//! **Water-bug docks.** The bug lands wherever its four probes 160 units
//! ahead find walkable cells whose zone is a dock value (6, 0x21, 0xC7–0xEF),
//! so a [`Dock`] paints one onto the shore tiles. Boarding (the Chester
//! flute on a dock cell) plays a cutscene whose four points come from a
//! 52-entry table in OVR02, picked by the value; entries 22 (0xD5, painted
//! nowhere in retail) and 39 (0xE3, three stray pit cells) are free, so a
//! project has two docks of its own: [`patch_docks`] writes their points.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::world::Prim;

use super::chunk::{Chunk, Group, CELLS};
use super::ground::{self, GroundMap, GroundTile, Material, Mesh, MaterialAt, BASE_Y, MAX_LEVEL, MIN_LEVEL, PAGE, STEP, TILE, TILES};
use super::unbake::Key;
use super::{EditError, Result};

/// The sea's palette on page 0x08.
pub const SEA_CLUT: u16 = 0x780A;

/// One tile of a [`LandMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandTile {
    /// A [`Material`] id 1–7; 0 leaves the tile as the chunk has it.
    pub material: u8,
    pub level: i8,
    /// 0 flat; 1–4 rises one level toward the north, east, south, west.
    #[serde(default)]
    pub ramp: u8,
    /// 16-unit steps (0–3) below its level: how land meets a retail beach (y −16 or −32) a step the Knight takes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub drop: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

impl LandTile {
    pub fn painted(&self) -> bool {
        self.material != 0
    }

    fn ground(&self) -> GroundTile {
        GroundTile { material: self.material, level: self.level, ramp: self.ramp }
    }
}

/// A grid chunk's new land: `TILES × TILES`, row-major from the south-west.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandMap {
    pub tiles: Vec<LandTile>,
    /// Layer B zone the land's cells take (0 keeps the chunk's).
    #[serde(default)]
    pub zone: u8,
    /// Water-bug docks on this chunk's new shores.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docks: Vec<Dock>,
}

impl Default for LandMap {
    fn default() -> Self {
        LandMap { tiles: vec![LandTile::default(); TILES * TILES], zone: 0, docks: Vec::new() }
    }
}

/// The zone values of a project's two docks, and their entries in OVR02's boarding table.
pub const DOCK_ZONES: [u8; 2] = [0xD5, 0xE3];
pub const DOCK_ENTRIES: [usize; 2] = [22, 39];
/// OVR02's boarding table (`D_801FC94C`): 52 entries of four chunk-local (x, z) points.
pub const DOCK_TABLE: usize = 0x801F_C94C - 0x801D_F000;
/// Entry 8 (zone 0x21) as retail has it: how [`patch_docks`] knows the table is where it expects.
const DOCK_PROBE: [i16; 8] = [648, -825, 518, -937, -386, -1120, 525, -1104];

/// A water-bug dock: the shore tile (painted land) and the side the water is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dock {
    /// 0 or 1: which of the project's docks ([`DOCK_ZONES`]).
    pub slot: u8,
    pub col: u8,
    pub row: u8,
    /// Toward the water: 0 north, 1 east, 2 south, 3 west.
    pub side: u8,
}

impl Dock {
    /// The zone value its cells take.
    pub fn zone(&self) -> u8 {
        DOCK_ZONES[self.slot as usize & 1]
    }

    /// Unit step (column, row) toward the water.
    fn out(&self) -> (i32, i32) {
        ground::TOWARD[self.side as usize & 3]
    }

    /// Its tiles: the shore tile and one each side along the shore, two deep.
    pub fn tiles(&self) -> Vec<(i32, i32)> {
        let (ox, oz) = self.out();
        let (ax, az) = (oz, ox);
        let mut out = Vec::new();
        for along in -1..=1 {
            for depth in 0..2 {
                out.push((self.col as i32 + ax * along - ox * depth, self.row as i32 + az * along - oz * depth));
            }
        }
        out
    }

    /// The boarding cutscene's points, chunk-local (x, z): where the Knight walks first, the shore
    /// (he runs there next), where the bug appears and where it stops (the Knight mounts there).
    /// Spaced like retail's: 170 inland, 190 out, the bug gliding in from 640 out.
    pub fn points(&self) -> [[i16; 2]; 4] {
        let (ox, oz) = self.out();
        let (ox, oz) = (ox as f32, oz as f32);
        // The middle of the shore edge, 40 units inland.
        let cx = corner_x(self.col as usize) as f32 + 64.0 + ox * 64.0 - ox * 40.0;
        let cz = corner_z(self.row as usize) as f32 + 64.0 + oz * 64.0 - oz * 40.0;
        let at = |d: f32| [(cx + ox * d).clamp(-2040.0, 2040.0) as i16, (cz + oz * d).clamp(-2040.0, 2040.0) as i16];
        [at(-170.0), at(0.0), at(640.0), at(190.0)]
    }
}

/// Writes the boarding points of `docks` (at most one per slot) into OVR02's
/// table; a slot with no dock gets retail's entry back from `retail`.
pub fn patch_docks(retail: &[u8], current: &[u8], docks: &[Dock]) -> Result<Vec<u8>> {
    let probe = |b: &[u8]| (0..8).all(|k| b.get(DOCK_TABLE + 16 * 8 + 2 * k..DOCK_TABLE + 16 * 8 + 2 * k + 2).is_some_and(|w| i16::from_le_bytes([w[0], w[1]]) == DOCK_PROBE[k]));
    if !probe(retail) || !probe(current) {
        return Err(EditError::new("DOCK_TABLE", "This game's OVR02 has no boarding table where retail has it, so docks can't be added."));
    }
    let mut out = current.to_vec();
    for (slot, entry) in DOCK_ENTRIES.iter().enumerate() {
        let at = DOCK_TABLE + 16 * entry;
        match docks.iter().find(|d| d.slot as usize == slot) {
            Some(d) => {
                for (k, p) in d.points().iter().enumerate() {
                    out[at + 4 * k..at + 4 * k + 2].copy_from_slice(&p[0].to_le_bytes());
                    out[at + 4 * k + 2..at + 4 * k + 4].copy_from_slice(&p[1].to_le_bytes());
                }
            }
            None => out[at..at + 16].copy_from_slice(&retail[at..at + 16]),
        }
    }
    Ok(out)
}

impl LandMap {
    pub fn get(&self, col: i32, row: i32) -> Option<LandTile> {
        ((0..TILES as i32).contains(&col) && (0..TILES as i32).contains(&row)).then(|| self.tiles[row as usize * TILES + col as usize])
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.iter().all(|t| !t.painted())
    }

    pub fn check(&self) -> Result<()> {
        if self.tiles.len() != TILES * TILES {
            return Err(EditError::new("LAND_SIZE", format!("A land map has {} tiles; this one has {}.", TILES * TILES, self.tiles.len())));
        }
        for t in self.tiles.iter().filter(|t| t.painted()) {
            if Material::from_id(t.material).is_none() {
                return Err(EditError::new("LAND_MATERIAL", format!("There is no ground material {}.", t.material)));
            }
            if !(MIN_LEVEL..=MAX_LEVEL).contains(&t.level) || (t.ramp > 0 && t.level >= MAX_LEVEL) || t.ramp > 4 || t.drop > 3 {
                return Err(EditError::new("LAND_LEVEL", format!("Levels go from {MIN_LEVEL} to {MAX_LEVEL} (a ramp rises one more), less up to three 16-unit drops.")));
            }
        }
        Ok(())
    }

    /// Surface y of a painted tile's corners: north-west, north-east, south-east, south-west.
    pub fn corners(t: LandTile) -> [i16; 4] {
        GroundMap::corners(t.ground()).map(|y| y + t.drop as i16 * 16)
    }
}

/// Chunk-local x of lattice column `i` (0–32): the east edge sits at +2051, as retail's does.
pub fn corner_x(i: usize) -> i16 {
    if i >= TILES { 2051 } else { -2048 + i as i16 * TILE }
}

/// Chunk-local z of lattice row `j` (0–32): the south edge sits at −2051.
pub fn corner_z(j: usize) -> i16 {
    if j == 0 { -2051 } else { -2048 + j as i16 * TILE }
}

/// The lattice index of a coordinate within 3 units of a lattice line (the edge overlap included).
fn lattice(v: i16) -> Option<usize> {
    let i = ((v as i32 + 2048) as f32 / TILE as f32).round() as i32;
    ((0..=TILES as i32).contains(&i) && (v as i32 - (-2048 + i * TILE as i32)).abs() <= 3).then_some(i as usize)
}

/// Whether `p` is a retail sea quad: FT4 on page 0x08 with the sea's palette, flat at y 0.
fn is_sea_quad(c: &Chunk, p: &Prim) -> bool {
    p.list == 3 && p.clut == SEA_CLUT && p.tpage & 0x1F == PAGE && p.idx.iter().all(|&i| c.pool.get(i as usize).is_some_and(|v| v[1] == 0))
}

/// The chunk's sea quads by the tile they cover exactly (row-major from the south-west).
pub fn sea_quads(c: &Chunk) -> BTreeMap<usize, Key> {
    let mut out = BTreeMap::new();
    for (g, group) in c.groups.iter().enumerate() {
        for (k, p) in group.prims.iter().enumerate().filter(|(_, p)| is_sea_quad(c, p)) {
            let xs: Vec<usize> = p.idx.iter().filter_map(|&i| lattice(c.pool[i as usize][0])).collect();
            let zs: Vec<usize> = p.idx.iter().filter_map(|&i| lattice(c.pool[i as usize][2])).collect();
            if xs.len() != 4 || zs.len() != 4 {
                continue;
            }
            let (x0, x1) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
            let (z0, z1) = (*zs.iter().min().unwrap(), *zs.iter().max().unwrap());
            if x1 == x0 + 1 && z1 == z0 + 1 {
                out.insert(z0 * TILES + x0, (g, k));
            }
        }
    }
    out
}

/// Which tiles are sea (the ones land can be painted on).
pub fn sea_tiles(c: &Chunk) -> Vec<bool> {
    let quads = sea_quads(c);
    (0..TILES * TILES).map(|t| quads.contains_key(&t)).collect()
}

/// What borders the chunk past its edge, at tile (column, row) of its frame
/// (a column or row of −1 or 32): the material painted there, if any, and
/// the surface y at that tile's corners (north-west, north-east, south-east,
/// south-west). `None` means the sea.
pub type Outside<'a> = &'a dyn Fn(i32, i32) -> Option<(Option<Material>, [i16; 4])>;

/// The top surface y at each lattice point of `c` (the least y among its vertices there).
fn surface(c: &Chunk) -> HashMap<(usize, usize), i16> {
    let mut out: HashMap<(usize, usize), i16> = HashMap::new();
    for v in &c.pool {
        if let (Some(i), Some(j)) = (lattice(v[0]), lattice(v[2])) {
            let e = out.entry((i, j)).or_insert(v[1]);
            *e = (*e).min(v[1]);
        }
    }
    out
}

/// The surface y at the corners (north-west, north-east, south-east, south-west) of an unpainted tile of `c`.
fn retail_corners(top: &HashMap<(usize, usize), i16>, sea: &BTreeMap<usize, Key>, col: usize, row: usize) -> [i16; 4] {
    if sea.contains_key(&(row * TILES + col)) {
        return [0; 4];
    }
    let at = |i: usize, j: usize| top.get(&(i, j)).copied().unwrap_or(0);
    [at(col, row + 1), at(col + 1, row + 1), at(col + 1, row), at(col, row)]
}

/// What [`apply`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LandReport {
    /// Sea tiles that became land.
    pub tiles: u32,
    pub cliffs: u32,
    /// Polygons the land added (tiles and cliffs).
    pub polygons: u32,
    /// Cells that took a dock's zone.
    pub dock_cells: u32,
}

/// Paints `map` over `c`'s sea (see the module notes); `outside` tells what
/// lies past the chunk's edges. Fails if a painted tile isn't sea, or the
/// chunk no longer fits its budgets.
pub fn apply(c: &mut Chunk, map: &LandMap, outside: Outside) -> Result<LandReport> {
    map.check()?;
    let mut report = LandReport::default();
    if map.is_empty() {
        return Ok(report);
    }
    let sea = sea_quads(c);
    if let Some(t) = (0..TILES * TILES).find(|t| map.tiles[*t].painted() && !sea.contains_key(t)) {
        return Err(EditError::new("LAND_NOT_SEA", format!("Tile {}, {} isn't sea: land is painted over the sea only.", t % TILES, t / TILES)));
    }
    let top = surface(c);
    // Drop the painted tiles' sea quads, then rebuild the pool from what is left.
    let gone: std::collections::HashSet<Key> = (0..TILES * TILES).filter(|t| map.tiles[*t].painted()).map(|t| sea[&t]).collect();
    let old = std::mem::take(&mut c.pool);
    let mut b = Mesh::default();
    let groups = std::mem::take(&mut c.groups);
    // Groups that lost sea quads get their culling centre again.
    let mut shrunk = Vec::new();
    for (g, mut group) in groups.into_iter().enumerate() {
        let n = group.prims.len();
        group.prims = group.prims.into_iter().enumerate().filter(|(k, _)| !gone.contains(&(g, *k))).map(|(_, p)| p).collect();
        if group.prims.len() != n && !group.prims.is_empty() {
            shrunk.push(c.groups.len());
        }
        for p in &mut group.prims {
            for k in 0..p.n as usize {
                p.idx[k] = b.vertex(old[p.idx[k] as usize]);
            }
        }
        if !group.prims.is_empty() {
            c.groups.push(group);
        }
    }
    let first_new = b.prims.len();
    let painted = |col: i32, row: i32| map.get(col, row).filter(|t| t.painted());
    let material = |col: i32, row: i32| match map.get(col, row) {
        Some(t) => t.painted().then(|| Material::from_id(t.material)).flatten(),
        None => outside(col, row).and_then(|o| o.0),
    };
    let material: MaterialAt = &material;
    // Corner heights of whatever is at (col, row): painted land, the chunk's own ground or sea, or past the edge.
    let heights = |col: i32, row: i32| -> [i16; 4] {
        if let Some(t) = painted(col, row) {
            return LandMap::corners(t);
        }
        if (0..TILES as i32).contains(&col) && (0..TILES as i32).contains(&row) {
            return retail_corners(&top, &sea, col as usize, row as usize);
        }
        outside(col, row).map_or([0; 4], |o| o.1)
    };
    for row in 0..TILES {
        for col in 0..TILES {
            let Some(t) = painted(col as i32, row as i32) else { continue };
            let m = Material::from_id(t.material).unwrap();
            let h = LandMap::corners(t);
            let (x0, x1, z0, z1) = (corner_x(col), corner_x(col + 1), corner_z(row), corner_z(row + 1));
            // North-west, north-east, south-east, south-west.
            let corner = [[x0, h[0], z1], [x1, h[1], z1], [x1, h[2], z0], [x0, h[3], z0]];
            match ground::look_with(material, col as i32, row as i32) {
                Some((tile, clut, turn, _)) => b.tile(corner, tile, clut, turn),
                None => b.flat(corner, [84, 116, 70]),
            }
            report.tiles += 1;
            // Rock where this tile stands above what is next to it.
            for (side, (dc, dr)) in ground::TOWARD.into_iter().enumerate() {
                let (a, bb) = [(1, 0), (2, 1), (3, 2), (0, 3)][side];
                let (na, nb) = [(2, 3), (3, 0), (0, 1), (1, 2)][side];
                let nh = heights(col as i32 + dc, row as i32 + dr);
                let (low_a, low_b) = (nh[na], nh[nb]);
                if h[a] < low_a || h[bb] < low_b {
                    report.cliffs += b.cliff([corner[a], corner[bb]], [low_a.max(h[a]), low_b.max(h[bb])], m.grassy_lip(), side);
                }
            }
        }
    }
    // The new polygons in draw groups of 512 units, like the retail ground.
    let added: Vec<Prim> = b.prims.drain(first_new..).collect();
    report.polygons = added.len() as u32;
    let mut blocks: Vec<((i32, i32), Group)> = Vec::new();
    for p in added {
        let n = p.n as usize;
        let cx = p.idx[..n].iter().map(|&i| b.pool[i as usize][0] as i32).sum::<i32>() / n as i32;
        let cz = p.idx[..n].iter().map(|&i| b.pool[i as usize][2] as i32).sum::<i32>() / n as i32;
        let key = ((cx + 2048).div_euclid(512), (cz + 2048).div_euclid(512));
        match blocks.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.prims.push(p),
            None => blocks.push((key, Group { centre: [0; 3], prims: vec![p] })),
        }
    }
    c.pool = b.pool;
    let first_group = c.groups.len();
    c.groups.extend(blocks.into_iter().map(|(_, g)| g));
    for g in shrunk.into_iter().chain(first_group..c.groups.len()) {
        c.recompute_centre(g);
    }
    // Walk heights and zones of the land's cells.
    for cell in 0..CELLS {
        let (col, row) = (cell % 128, cell / 128);
        let Some(t) = painted((col / 4) as i32, (row / 4) as i32) else { continue };
        if t.material == Material::Water as u8 {
            c.heights[cell] = -1;
            continue;
        }
        let h = LandMap::corners(t);
        let (fx, fz) = (((col % 4) as f32 + 0.5) / 4.0, ((row % 4) as f32 + 0.5) / 4.0);
        let south = h[3] as f32 * (1.0 - fx) + h[2] as f32 * fx;
        let north = h[0] as f32 * (1.0 - fx) + h[1] as f32 * fx;
        c.heights[cell] = (south * (1.0 - fz) + north * fz).round() as i16 - 1;
        if map.zone != 0 {
            c.zones[cell] = map.zone;
        }
    }
    // Docks: their shore tiles take the dock's zone, with open water in front.
    for d in &map.docks {
        let water = |col: i32, row: i32| match painted(col, row) {
            Some(t) => t.material == Material::Water as u8,
            None => heights(col, row) == [0; 4],
        };
        let (ox, oz) = d.out();
        let shore = painted(d.col as i32, d.row as i32).is_some_and(|t| t.material != Material::Water as u8);
        if !shore || !(1..=3).all(|k| water(d.col as i32 + ox * k, d.row as i32 + oz * k)) {
            return Err(EditError::new("DOCK_SHORE", format!("A dock goes on new land at the shore, with open water three tiles out (tile {}, {}).", d.col, d.row)));
        }
        for (col, row) in d.tiles() {
            if painted(col, row).is_none_or(|t| t.material == Material::Water as u8) {
                continue;
            }
            for k in 0..16 {
                let cell = (row as usize * 4 + k / 4) * 128 + col as usize * 4 + k % 4;
                c.zones[cell] = d.zone();
            }
            report.dock_cells += 16;
        }
    }
    c.sizes()?;
    Ok(report)
}

/// The grid chunk holding tile (`col`, `row`) of chunk `id`'s frame, a tile
/// past its edge (a column or row of −1 or 32): that chunk's id and the
/// tile's column and row there. Grid tiles line up across every seam (a
/// chunk is 32 tiles wide, the rows are offset by 16), and the overworld
/// wraps. `None` for interiors and new areas, which have no neighbours.
pub fn across(id: u32, col: i32, row: i32) -> Option<(u32, usize, usize)> {
    let (gx, gy) = crate::world::grid_cell(id)?;
    let (gx, gy) = (gx as i32, gy as i32);
    // World tile of (col, row): chunk origins are (2048·gx, −4096·gy), tile 0 starts 2048 west and south.
    let (tx, tz) = (16 * gx - 16 + col, -32 * gy - 16 + row);
    for (dx, dy) in [(0, 0), (2, 0), (-2, 0), (1, 1), (-1, 1), (1, -1), (-1, -1)] {
        let (nx, ny) = (gx + dx, gy + dy);
        let (c, r) = (tx - (16 * nx - 16), tz - (-32 * ny - 16));
        if (0..TILES as i32).contains(&c) && (0..TILES as i32).contains(&r) {
            let n = crate::world::chunk_at(nx.rem_euclid(36) as u32, ny.rem_euclid(18) as u32)?;
            return Some((n, c as usize, r as usize));
        }
    }
    None
}

/// What [`Outside`] needs to know about a neighbouring chunk: its sea tiles
/// and ground before any land (`base`), and the land painted on it.
pub struct Neighbour {
    sea: BTreeMap<usize, Key>,
    top: HashMap<(usize, usize), i16>,
    pub map: Option<LandMap>,
}

impl Neighbour {
    pub fn new(base: &Chunk, map: Option<LandMap>) -> Self {
        Neighbour { sea: sea_quads(base), top: surface(base), map }
    }

    /// The material painted at its tile (col, row) and the surface at that tile's corners; `None` for its sea.
    pub fn at(&self, col: usize, row: usize) -> Option<(Option<Material>, [i16; 4])> {
        if let Some(t) = self.map.as_ref().and_then(|m| m.get(col as i32, row as i32)).filter(|t| t.painted()) {
            return Some((Material::from_id(t.material), LandMap::corners(t)));
        }
        if self.sea.contains_key(&(row * TILES + col)) {
            return None;
        }
        Some((None, retail_corners(&self.top, &self.sea, col, row)))
    }
}

/// The base height of land (level 0), for the editor.
pub const LAND_Y: i16 = BASE_Y;
/// Units per level.
pub const LEVEL: i16 = STEP;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::chunk::Group;

    /// Open sea, as retail builds it: 32 × 32 FT4 sea quads at y 0, walk −1, zone 241.
    pub(crate) fn sea_chunk() -> Chunk {
        let mut c = Chunk::blank(2);
        for j in 0..=TILES {
            for i in 0..=TILES {
                c.pool.push([corner_x(i), 0, corner_z(j)]);
            }
        }
        let v = |i: usize, j: usize| (j * (TILES + 1) + i) as u16;
        let mut g = Group { centre: [0; 3], prims: Vec::new() };
        for j in 0..TILES {
            for i in 0..TILES {
                g.prims.push(Prim {
                    list: 3,
                    code: 0x2C,
                    n: 4,
                    idx: [v(i, j + 1), v(i + 1, j + 1), v(i, j), v(i + 1, j)],
                    rgb: [[128; 3]; 4],
                    uv: [[191, 192], [191, 223], [160, 192], [160, 223]],
                    clut: SEA_CLUT,
                    tpage: PAGE,
                });
            }
        }
        c.groups.push(g);
        c.recompute_centre(0);
        c.heights.iter_mut().for_each(|h| *h = -1);
        c.zones.iter_mut().for_each(|z| *z = 241);
        c
    }

    fn island(cols: std::ops::Range<usize>, rows: std::ops::Range<usize>, level: i8) -> LandMap {
        let mut m = LandMap { zone: 0x60, ..LandMap::default() };
        for r in rows {
            for c in cols.clone() {
                m.tiles[r * TILES + c] = LandTile { material: Material::Grass as u8, level, ramp: 0, drop: 0 };
            }
        }
        m
    }

    #[test]
    fn sea_is_found_tile_by_tile() {
        let c = sea_chunk();
        assert!(sea_tiles(&c).iter().all(|&s| s));
        assert_eq!(sea_quads(&c).len(), TILES * TILES);
    }

    #[test]
    fn an_island_replaces_its_sea_and_stands_on_cliffs() {
        let mut c = sea_chunk();
        let before = c.groups[0].prims.len();
        let r = apply(&mut c, &island(10..14, 10..14, 0), &|_, _| None).unwrap();
        assert_eq!(r.tiles, 16);
        // 16 edge sides, each 128 units over the sea: two 64-unit bands.
        assert_eq!(r.cliffs, 32);
        assert_eq!(c.groups[0].prims.len(), before - 16);
        assert_eq!(sea_tiles(&c).iter().filter(|s| !**s).count(), 16);
        // Its walk cells are lowland (−129) with the land's zone; the sea around stays blocked.
        let cell = |col: usize, row: usize| row * 128 + col;
        assert_eq!((c.heights[cell(45, 45)], c.zones[cell(45, 45)]), (-129, 0x60));
        assert_eq!((c.heights[cell(30, 30)], c.zones[cell(30, 30)]), (-1, 241));
        c.serialize().unwrap();
    }

    #[test]
    fn a_hill_on_the_island_takes_its_own_cliffs() {
        let mut c = sea_chunk();
        let mut m = island(10..16, 10..16, 0);
        for r in 12..14 {
            for col in 12..14 {
                m.tiles[r * TILES + col].level = 2;
            }
        }
        let r = apply(&mut c, &m, &|_, _| None).unwrap();
        // The shore: 24 sides × 2 bands; the hill: 8 sides × 2 bands.
        assert_eq!(r.cliffs, 48 + 16);
        c.serialize().unwrap();
    }

    #[test]
    fn the_edge_meets_its_neighbour() {
        let mut c = sea_chunk();
        // Land on the east edge; past it, the neighbour has land at the same level: no cliff there.
        let m = island(28..32, 10..12, 0);
        let same = |col: i32, _row: i32| (col >= TILES as i32).then_some((Some(Material::Grass), [LAND_Y; 4]));
        let r = apply(&mut c, &m, &same).unwrap();
        // 4 × 2 tiles: north and south sides (8) and the west end (2), two bands each; no east face.
        assert_eq!(r.cliffs, 20);
        // The east edge's vertices sit at +2051, like retail's.
        assert!(c.pool.iter().any(|v| v[0] == 2051 && v[1] == LAND_Y));
        let mut c2 = sea_chunk();
        assert_eq!(apply(&mut c2, &m, &|_, _| None).unwrap().cliffs, 24);
    }

    #[test]
    fn land_goes_on_sea_only() {
        let mut c = sea_chunk();
        apply(&mut c, &island(0..2, 0..2, 0), &|_, _| None).unwrap();
        // Painting again over what is now land is refused (the scene always paints the base).
        assert_eq!(apply(&mut c, &island(0..1, 0..1, 0), &|_, _| None).unwrap_err().code, "LAND_NOT_SEA");
        let mut bad = LandMap::default();
        bad.tiles[0] = LandTile { material: 9, level: 0, ramp: 0, drop: 0 };
        assert_eq!(apply(&mut sea_chunk(), &bad, &|_, _| None).unwrap_err().code, "LAND_MATERIAL");
    }

    #[test]
    fn a_dock_takes_its_zone_and_its_points_face_the_water() {
        let mut c = sea_chunk();
        let mut m = island(10..16, 10..16, 0);
        // On the east shore (column 15), water to the east.
        m.docks.push(Dock { slot: 0, col: 15, row: 12, side: 1 });
        let r = apply(&mut c, &m, &|_, _| None).unwrap();
        // 3 along the shore × 2 deep, all land.
        assert_eq!(r.dock_cells, 6 * 16);
        assert_eq!(c.zones[(12 * 4 + 1) * 128 + 15 * 4 + 1], 0xD5);
        assert_eq!(c.zones[(12 * 4 + 1) * 128 + 13 * 4 + 1], 0x60);
        let p = m.docks[0].points();
        // Shore edge x = −2048 + 16·128 = 0; the shore point is 40 inland, the approach 170 more, the bug out east.
        assert_eq!(p[1], [-40, -2048 + 12 * 128 + 64]);
        assert_eq!(p[0][0], -210);
        assert!(p[3][0] == 150 && p[2][0] == 600);
        // A dock facing land is refused.
        let mut bad = island(10..16, 10..16, 0);
        bad.docks.push(Dock { slot: 0, col: 12, row: 12, side: 1 });
        assert_eq!(apply(&mut sea_chunk(), &bad, &|_, _| None).unwrap_err().code, "DOCK_SHORE");
    }

    #[test]
    fn docks_patch_the_boarding_table_and_give_it_back() {
        let mut retail = vec![0u8; 0x20000];
        for (k, v) in DOCK_PROBE.iter().enumerate() {
            retail[DOCK_TABLE + 16 * 8 + 2 * k..][..2].copy_from_slice(&v.to_le_bytes());
        }
        retail[DOCK_TABLE + 16 * 39] = 7;
        let d = Dock { slot: 0, col: 15, row: 12, side: 1 };
        let out = patch_docks(&retail, &retail, &[d]).unwrap();
        let at = DOCK_TABLE + 16 * 22;
        assert_eq!(i16::from_le_bytes([out[at + 4], out[at + 5]]), d.points()[1][0]);
        // Slot 1 has no dock: retail's entry stays; taking the dock away gives entry 22 back.
        assert_eq!(out[DOCK_TABLE + 16 * 39], 7);
        assert_eq!(patch_docks(&retail, &out, &[]).unwrap(), retail);
        assert_eq!(patch_docks(&vec![0u8; 0x20000], &retail, &[d]).unwrap_err().code, "DOCK_TABLE");
    }

    #[test]
    fn tiles_past_the_edge_belong_to_the_neighbours() {
        // 136 is grid (28, 6); 138 is east of it, 137 and 135 south (offset half a chunk), 99 north-west.
        assert_eq!(across(136, 32, 5), Some((138, 0, 5)));
        assert_eq!(across(136, 20, -1), Some((137, 4, 31)));
        assert_eq!(across(136, 10, -1), Some((135, 26, 31)));
        assert_eq!(across(136, 10, 32), Some((99, 26, 0)));
        assert_eq!(across(136, 3, 3), Some((136, 3, 3)));
        // The overworld wraps: chunk 0 is grid (0, 0); west of it is grid (34, 0).
        assert_eq!(across(0, -1, 3), Some((34, 31, 3)));
        assert_eq!(across(330, 32, 0), None);
    }

    #[test]
    fn a_drop_meets_a_beach_a_step_at_a_time() {
        // A strip climbing from a beach (y −16) to the lowland (−128): 32 units a tile.
        let mut c = sea_chunk();
        let mut m = LandMap::default();
        for (k, (level, drop)) in [(-1, 3), (-1, 1), (0, 3), (0, 1), (0, 0)].into_iter().enumerate() {
            m.tiles[10 * TILES + 10 + k] = LandTile { material: Material::Sand as u8, level, ramp: 0, drop };
        }
        apply(&mut c, &m, &|_, _| None).unwrap();
        let walk: Vec<i16> = (0..5).map(|k| c.heights[(10 * 4 + 1) * 128 + (10 + k) * 4 + 1]).collect();
        assert_eq!(walk, vec![-17, -49, -81, -113, -129]);
        // Every step between tiles is 32 or less: ones the Knight takes.
        assert!(walk.windows(2).all(|w| (w[0] - w[1]).abs() <= 32));
        c.serialize().unwrap();
    }

    #[test]
    fn an_empty_map_changes_nothing() {
        let mut c = sea_chunk();
        let before = c.serialize().unwrap();
        apply(&mut c, &LandMap::default(), &|_, _| None).unwrap();
        assert_eq!(c.serialize().unwrap(), before);
    }
}
