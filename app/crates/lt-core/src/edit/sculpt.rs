//! Sculpting the ground of a grid chunk (screen 10's Sculpt tool): raising and
//! lowering the terrain and what stands on it, in step with the neighbours.
//!
//! Every overworld chunk's terrain lies on one world-wide lattice of 128-unit
//! tiles, and each chunk's edge has its vertices on the same lattice spots as
//! the neighbour's edge (the east and south edges at +2051 and −2051, three
//! units over the neighbour's). A [`SculptMap`] is a rise at each of a
//! chunk's 33 × 33 lattice points; between points it is bilinear. The editor
//! samples one world-space brush for every chunk a stroke reaches, so two
//! chunks hold the same rise at the points they share, and [`apply`] clamps
//! an edge vertex to the edge point: the seam moves as one.
//!
//! What moves:
//! - terrain, the ground decals no piece owns and *fixed* pieces (cliffs,
//!   rivers and walls running into the neighbours), vertex by vertex with
//!   the ground under each;
//! - every other piece ([`super::unbake::split`]: a house, a tree, a sign)
//!   whole, by the rise under its centre, so it stays upright and level,
//!   except where it touches the ground: the pool is deduplicated by
//!   position, so a piece shares the corners of its ground patch and its
//!   feet with the terrain around it, and those keep the ground's rise (the
//!   piece stretches a little to meet it rather than open a crack);
//! - the walk heights, by the rise under each cell's centre in whole steps
//!   of [`WALK_STEP`] (retail ramps are 16-unit staircases, and runs of
//!   equal heights keep layer A's run-length data small). Blocked cells (−1)
//!   and pits (0) stay; a walkable cell lowered to the plane stops at −2.
//!
//! Draw groups whose vertices moved get their culling centre again.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::chunk::{Chunk, CELLS};
use super::unbake::{Key, Split};
use super::{EditError, Result};

/// Lattice points per side: tiles −2048..+2048 every 128 units.
pub const POINTS: usize = 33;
/// Units between lattice points.
pub const SPACING: i32 = 128;
/// Walk heights move in whole steps of this.
pub const WALK_STEP: i32 = 16;
/// The highest a walkable cell may stand after a lowering (−1 blocks, 0 is a pit).
const LOWEST_WALK: i32 = -2;
/// The most a point may rise or sink.
pub const MAX_RISE: i16 = 2048;

/// Rises (units up, the opposite of PSX Y) at a chunk's lattice points,
/// row-major from the south-west: point (i, j) is at local x = −2048 + 128·i,
/// z = −2048 + 128·j.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SculptMap {
    pub points: Vec<i16>,
}

impl SculptMap {
    pub fn flat() -> Self {
        SculptMap { points: vec![0; POINTS * POINTS] }
    }

    pub fn is_flat(&self) -> bool {
        self.points.iter().all(|&p| p == 0)
    }

    fn check(&self) -> Result<()> {
        if self.points.len() != POINTS * POINTS {
            return Err(EditError::new("SCULPT_SIZE", format!("A sculpt map has {} points ({POINTS} × {POINTS}); this one has {}.", POINTS * POINTS, self.points.len())));
        }
        if let Some(p) = self.points.iter().find(|p| p.unsigned_abs() > MAX_RISE as u16) {
            return Err(EditError::new("SCULPT_RANGE", format!("A point rises {p}; the most is ±{MAX_RISE}.")));
        }
        Ok(())
    }

    fn point(&self, i: usize, j: usize) -> f32 {
        self.points[j * POINTS + i] as f32
    }

    /// The rise at chunk-local (x, z), bilinear between the points. Points
    /// past the lattice (the three-unit overlap at +2051 / −2051) take the
    /// edge's rise.
    pub fn at(&self, x: f32, z: f32) -> f32 {
        let last = (POINTS - 1) as f32;
        let u = ((x + 2048.0) / SPACING as f32).clamp(0.0, last);
        let v = ((z + 2048.0) / SPACING as f32).clamp(0.0, last);
        let (i, j) = ((u.floor() as usize).min(POINTS - 2), (v.floor() as usize).min(POINTS - 2));
        let (fu, fv) = (u - i as f32, v - j as f32);
        let a = self.point(i, j) * (1.0 - fu) + self.point(i + 1, j) * fu;
        let b = self.point(i, j + 1) * (1.0 - fu) + self.point(i + 1, j + 1) * fu;
        a * (1.0 - fv) + b * fv
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SculptReport {
    /// Pool vertices that moved.
    pub vertices: u32,
    /// Whole pieces raised or lowered.
    pub pieces: u32,
    /// Walk cells whose height changed.
    pub cells: u32,
    /// Walkable cells lowered as far as they go (−2).
    pub clamped: u32,
}

/// Rounds `d` to whole walk steps (half away from zero).
fn walk_rise(d: f32) -> i32 {
    let s = WALK_STEP as f32;
    ((d / s).abs() + 0.5).floor().copysign(d) as i32 * WALK_STEP
}

/// Raises and lowers `c` by `map` (see the module notes). `split` is
/// [`super::unbake::split`] of `c` as it is now; the polygons keep their
/// order, so its keys still name them afterwards.
pub fn apply(c: &mut Chunk, map: &SculptMap, split: &Split) -> Result<SculptReport> {
    map.check()?;
    let mut report = SculptReport::default();
    if map.is_flat() {
        return Ok(report);
    }
    // Whole pieces: the rise under their centre.
    let mut whole: HashMap<Key, i16> = HashMap::new();
    for p in split.pieces.iter().filter(|p| !p.fixed) {
        let (x, z) = p.centre();
        let d = map.at(x as f32, z as f32).round() as i16;
        if d != 0 {
            report.pieces += 1;
        }
        for &k in &p.prims {
            whole.insert(k, d);
        }
    }
    // The ground's vertices: every vertex a polygon of no whole piece uses.
    let mut on_ground = vec![false; c.pool.len()];
    for (g, group) in c.groups.iter().enumerate() {
        for (_, p) in group.prims.iter().enumerate().filter(|(k, _)| !whole.contains_key(&(g, *k))) {
            for &i in &p.idx[..p.n as usize] {
                on_ground[i as usize] = true;
            }
        }
    }
    let mut rise: Vec<Option<i16>> = vec![None; c.pool.len()];
    let mut moved_groups = vec![false; c.groups.len()];
    for (g, group) in c.groups.iter().enumerate() {
        for (k, p) in group.prims.iter().enumerate() {
            for &i in &p.idx[..p.n as usize] {
                let slot = &mut rise[i as usize];
                if slot.is_none() {
                    let v = c.pool[i as usize];
                    *slot = Some(match whole.get(&(g, k)) {
                        Some(&d) if !on_ground[i as usize] => d,
                        _ => map.at(v[0] as f32, v[2] as f32).round() as i16,
                    });
                }
                if *slot != Some(0) {
                    moved_groups[g] = true;
                }
            }
        }
    }
    for (v, d) in c.pool.iter_mut().zip(&rise) {
        if let Some(d) = d.filter(|&d| d != 0) {
            v[1] = (v[1] as i32 - d as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            report.vertices += 1;
        }
    }
    for g in (0..c.groups.len()).filter(|&g| moved_groups[g]) {
        c.recompute_centre(g);
    }
    // Walk heights: the rise under each cell's centre, in whole steps.
    for cell in 0..CELLS.min(c.heights.len()) {
        let h = c.heights[cell] as i32;
        if h >= 0 || h == -1 {
            continue;
        }
        let (col, row) = ((cell % 128) as f32, (cell / 128) as f32);
        let d = walk_rise(map.at(col * 32.0 - 2048.0 + 16.0, row * 32.0 - 2048.0 + 16.0));
        if d == 0 {
            continue;
        }
        let mut to = h - d;
        if to > LOWEST_WALK {
            to = LOWEST_WALK;
            report.clamped += 1;
        }
        if to != h {
            c.heights[cell] = to.max(i16::MIN as i32) as i16;
            report.cells += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::unbake::Piece;
    use crate::world::Prim;

    fn no_split() -> Split {
        Split { pieces: Vec::new(), ground: Vec::new(), kinds: Default::default() }
    }

    fn point_map(i: usize, j: usize, d: i16) -> SculptMap {
        let mut m = SculptMap::flat();
        m.points[j * POINTS + i] = d;
        m
    }

    fn quad(idx: [u16; 4]) -> Prim {
        Prim { list: 2, code: 0x28, n: 4, idx, rgb: [[128; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 }
    }

    #[test]
    fn a_flat_map_changes_nothing() {
        let mut c = Chunk::new_area(1328);
        let before = c.serialize().unwrap();
        let r = apply(&mut c, &SculptMap::flat(), &no_split()).unwrap();
        assert_eq!(r, SculptReport::default());
        assert_eq!(c.serialize().unwrap(), before);
    }

    #[test]
    fn a_point_lifts_the_ground_and_the_walk_cells_in_steps() {
        let mut c = Chunk::new_area(1328);
        // The point at local (0, 0) is lattice (16, 16), a corner of the new area's 512-unit quads.
        let r = apply(&mut c, &point_map(16, 16, 64), &no_split()).unwrap();
        let v = c.pool.iter().find(|v| v[0] == 0 && v[2] == 0).unwrap();
        assert_eq!(v[1], -128 - 64);
        // The quads' other corners are 512 away, outside the point's reach: they stay.
        assert!(c.pool.iter().filter(|v| v[0] != 0 || v[2] != 0).all(|v| v[1] == -128));
        assert_eq!(r.vertices, 1);
        // The cell whose centre is 16 units off the point rises 64·(1 − 16/128)² ≈ 49 → 48.
        let cell = |x: i32, z: i32| (((z + 2048) >> 5) * 128 + ((x + 2048) >> 5)) as usize;
        assert_eq!(c.heights[cell(0, 0)], -129 - 48);
        // Cells a tile away don't move, and the walls of the outer ring (0) never do.
        assert_eq!(c.heights[cell(200, 0)], -129);
        assert!(r.cells > 0 && r.clamped == 0);
        c.serialize().unwrap();
    }

    #[test]
    fn edges_take_the_edge_points_rise() {
        let m = point_map(32, 10, 32);
        // The east edge at +2051 (three units into the neighbour) reads the point on +2048.
        assert_eq!(m.at(2051.0, -2048.0 + 1280.0), 32.0);
        assert_eq!(m.at(2048.0, -2048.0 + 1280.0), 32.0);
        assert_eq!(m.at(-2051.0, 0.0), 0.0);
        // Halfway between two points, half the rise.
        assert_eq!(m.at(2048.0, -2048.0 + 1280.0 + 64.0), 16.0);
    }

    #[test]
    fn a_piece_rides_whole_and_its_feet_stay_on_the_ground() {
        // Ground: one 128 tile at (0..128, 0..128), y −100. A prop: a wall standing on its west edge, sharing
        // its two feet with the ground's corners (the pool is deduplicated by position).
        let wall = || {
            let mut c = Chunk::blank(10);
            c.pool = vec![[0, -100, 0], [128, -100, 0], [0, -100, 128], [128, -100, 128], [0, -164, 0], [0, -164, 128]];
            c.groups.push(crate::edit::chunk::Group { centre: [0; 3], prims: vec![quad([2, 3, 0, 1]), quad([5, 2, 4, 0])] });
            c.recompute_centre(0);
            c
        };
        let split = Split {
            pieces: vec![Piece { prims: vec![(0, 1)], decals: 0, min: [0, -164, 0], max: [0, -100, 128], fixed: false, cut: None }],
            ground: Vec::new(),
            kinds: Default::default(),
        };
        // Lattice point (17, 16) is local (128, 0): only the tile's east corner rises; the wall's centre
        // (0, 64) is a tile from it and stays.
        let mut c = wall();
        let r = apply(&mut c, &point_map(17, 16, 32), &split).unwrap();
        assert_eq!((r.pieces, c.pool[1][1], c.pool[4][1]), (0, -132, -164));
        // Raise the corner under the wall's south foot (0, 0) by 64: its centre reads half, 32.
        let mut c = wall();
        let r = apply(&mut c, &point_map(16, 16, 64), &split).unwrap();
        assert_eq!(r.pieces, 1);
        // The wall's top rises 32 and stays level; its feet stay on the ground (64 and 0): no crack, no copies.
        assert_eq!(c.pool, vec![[0, -164, 0], [128, -100, 0], [0, -100, 128], [128, -100, 128], [0, -196, 0], [0, -196, 128]]);
        c.serialize().unwrap();
    }

    #[test]
    fn lowering_stops_walkable_cells_above_the_plane() {
        let mut c = Chunk::new_area(1328);
        let mut m = SculptMap::flat();
        m.points.iter_mut().for_each(|p| *p = -400);
        let r = apply(&mut c, &m, &no_split()).unwrap();
        assert!(c.heights.iter().all(|&h| h == 0 || h == -2));
        assert_eq!(r.clamped, r.cells);
        assert!(c.pool.iter().all(|v| v[1] == -128 + 400));
    }

    #[test]
    fn a_bad_map_is_refused() {
        let mut c = Chunk::new_area(1328);
        assert_eq!(apply(&mut c, &SculptMap { points: vec![0; 10] }, &no_split()).unwrap_err().code, "SCULPT_SIZE");
        assert_eq!(apply(&mut c, &point_map(1, 1, 5000), &no_split()).unwrap_err().code, "SCULPT_RANGE");
    }
}
