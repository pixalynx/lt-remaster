//! Where the Knight can walk, from layer A alone (research 07,
//! `Overworld_KnightUpdate` main 0x801942C4, `ScanFieldEventTriggers` 0x8018FFF8).
//!
//! Walls are walk heights. An interior's floor is one height (−33 in the
//! Knight's house, chunk 326) and its walls and partitions are cells raised far
//! above it (−225 there): a flat, walkable-looking plateau the Knight can never
//! climb onto, because every step onto it is more than 0x20. Heights −1 and 0
//! are no ground at all (`GetTerrainHeightAt` gives 0 where nothing was baked).
//!
//! The game's rules, as the decomp has them:
//! - a step goes ahead only when the cell under the Knight's new centre is within
//!   [`STEP`] (0x20) of his height;
//! - before it, a probe [`REACH`] (0x20) units ahead of his centre on each moving
//!   axis, and to either side when he moves along one axis, slides him back to
//!   0x20 short of any cell that is −1, 0 or more than [`PROBE`] (0x30) off his
//!   height. So his centre keeps 32 units from a wall;
//! - a door box or a scene spot fires when his centre is within [`BOX`] (0x20) of
//!   it on both axes.
//!
//! [`Walk`] classifies each cell ([`Cell::Floor`], [`Cell::Wall`], [`Cell::Void`])
//! and samples where his centre can be on a 16-unit lattice, which is exact here:
//! every limit falls on a multiple of 32.

use serde::Serialize;

use super::heightmap;

/// The largest height difference a step may climb or drop.
pub const STEP: i32 = 0x20;
/// A probed cell further than this off the Knight's height is a wall.
pub const PROBE: i32 = 0x30;
/// How far ahead of his centre the probe looks.
pub const REACH: i32 = 0x20;
/// A trigger box's half size: it fires within this of its centre on both axes.
pub const BOX: i32 = 0x20;

const N: usize = 128;
/// The standing lattice: 256 × 256 points 16 units apart, from −2048.
const L: usize = 256;
const LSTEP: i32 = 16;
const NONE: u32 = u32::MAX;

/// What a cell is to the Knight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Cell {
    /// Ground he can stand on.
    Floor,
    /// Ground raised above everything around it by more than a step: the top of a
    /// wall, a partition or tall furniture. Nothing can get onto it on foot.
    Wall,
    /// No ground: −1, 0 (or above 0, which retail never has).
    Void,
}

/// No ground: −1 blocks, and the game reads 0 as "nothing baked here".
pub fn is_void(h: i16) -> bool {
    h == -1 || h >= 0
}

/// Why a spot is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// Outside the chunk (−2048…2047).
    Outside,
    /// The cell has no ground.
    Void { col: u32, row: u32, h: i16 },
    /// The cell is a wall top, `rise` units above the floor beside it.
    Wall { col: u32, row: u32, h: i16, rise: Option<i32> },
    /// A box the Knight's centre can't get within [`BOX`] of. `nearest` is the
    /// closest point he can stand on, `gap` how far that is from the box (on the
    /// longer axis), `fit` the box moved just far enough to reach it.
    OutOfReach { nearest: Option<(i16, i16)>, gap: Option<i32>, fit: Option<(i16, i16)> },
}

impl Blocked {
    /// A sentence for a refusal about the spot (`x`, `z`).
    pub fn message(&self, x: i16, z: i16) -> String {
        match *self {
            Blocked::Outside => format!("({x}, {z}) is outside the chunk (−2048…2047)."),
            Blocked::Void { col, row, h } => format!("({x}, {z}), cell {col}, {row}, has no ground (height {h})."),
            Blocked::Wall { col, row, h, rise } => match rise {
                Some(r) => format!(
                    "({x}, {z}), cell {col}, {row}, is the top of a wall (height {h}, {r} units above the floor beside it). The Knight climbs at most {STEP}, so he can't get onto it."
                ),
                None => format!("({x}, {z}), cell {col}, {row}, is the top of a wall (height {h}). The Knight can't get onto it."),
            },
            Blocked::OutOfReach { nearest: Some((nx, nz)), gap: Some(g), fit } => {
                let fit = fit.map_or(String::new(), |(fx, fz)| format!(" Move it to ({fx}, {fz}), or nearer the floor."));
                format!(
                    "The Knight can't reach a box at ({x}, {z}): walls keep him {g} units from it, and it fires within {BOX}. The nearest he can stand is ({nx}, {nz}).{fit}"
                )
            }
            Blocked::OutOfReach { .. } => format!("The Knight can't reach a box at ({x}, {z}): there is no floor he can stand on near it."),
        }
    }
}

/// Layer A read for walking: cell classes, and where the Knight's centre can be.
#[derive(Debug, Clone)]
pub struct Walk {
    heights: Vec<i16>,
    cells: Vec<Cell>,
    /// Cells joined by steps of ≤ [`STEP`]; [`NONE`] for void cells.
    comp: Vec<u32>,
    /// The standing region of each lattice point (points joined by walking); [`NONE`] where he can't stand.
    region: Vec<u32>,
    /// A grid chunk: its edges carry on into the neighbours.
    open: bool,
}

impl Walk {
    /// The walk map of chunk `id` (a 0x3E800-byte chunk file), with the points
    /// the Knight is known to arrive at (`entries`, see [`Walk::new`]).
    pub fn of_chunk(id: u32, chunk: &[u8], entries: &[(i16, i16)]) -> Self {
        Self::new(&heightmap(chunk), id < super::GRID_CHUNKS, entries)
    }

    /// The walk map of 128 × 128 heights (row = z cell). `open` for a grid chunk,
    /// whose edges continue into the neighbouring chunks: ground there counts as
    /// reachable from outside, and probes past the edge as clear. `entries` are
    /// points the Knight arrives at (door arrivals, the start): ground they land
    /// on is floor even when nothing leads up to it, as a level reached only by a
    /// door is (chunk 333's upper floor, 128 above the lower one).
    pub fn new(heights: &[i16], open: bool, entries: &[(i16, i16)]) -> Self {
        let mut h = heights.to_vec();
        h.resize(N * N, -1);
        let (comp, ncomp) = components(&h);
        let seeded: Vec<u32> = entries.iter().filter_map(|&(x, z)| cell_of(x as i32, z as i32)).map(|(c, r)| comp[r as usize * N + c as usize]).filter(|&c| c != NONE).collect();
        let cells = classify(&h, &comp, ncomp, open, &seeded);
        let mut w = Walk { heights: h, cells, comp, region: vec![NONE; L * L], open };
        w.region = w.regions();
        w
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn cell(&self, col: u32, row: u32) -> Cell {
        if col as usize >= N || row as usize >= N {
            return Cell::Void;
        }
        self.cells[row as usize * N + col as usize]
    }

    pub fn height(&self, col: u32, row: u32) -> i16 {
        self.heights.get(row as usize * N + col as usize).copied().unwrap_or(-1)
    }

    /// Wall cells.
    pub fn walls(&self) -> usize {
        self.cells.iter().filter(|c| **c == Cell::Wall).count()
    }

    /// The floor height at (`x`, `z`), for an actor, a scene director or an
    /// arrival: the cell must be floor.
    pub fn stand(&self, x: i16, z: i16) -> Result<i16, Blocked> {
        let (col, row) = cell_of(x as i32, z as i32).ok_or(Blocked::Outside)?;
        let h = self.height(col, row);
        match self.cell(col, row) {
            Cell::Floor => Ok(h),
            Cell::Void => Err(Blocked::Void { col, row, h }),
            Cell::Wall => Err(Blocked::Wall { col, row, h, rise: self.rise(col, row) }),
        }
    }

    /// Whether the Knight's centre can be at (`x`, `z`), units.
    pub fn can_stand(&self, x: i32, z: i32) -> bool {
        self.standing(x, z).is_some()
    }

    /// The standing regions inside the ±[`BOX`] box at (`x`, `z`): the places from
    /// which walking into it fires it. Refused when there are none.
    pub fn reach_box(&self, x: i16, z: i16) -> Result<Vec<u32>, Blocked> {
        if !(-0x800..0x800).contains(&x) || !(-0x800..0x800).contains(&z) {
            return Err(Blocked::Outside);
        }
        let (x, z) = (x as i32, z as i32);
        let mut out = Vec::new();
        for (px, pz) in lattice_in(x - BOX, x + BOX, z - BOX, z + BOX) {
            let r = self.region[lat(px, pz)];
            if r != NONE && !out.contains(&r) {
                out.push(r);
            }
        }
        if out.is_empty() {
            let nearest = self.nearest_standing(x, z, 0x400);
            let gap = nearest.map(|(nx, nz)| (nx as i32 - x).abs().max((nz as i32 - z).abs()));
            let fit = nearest.map(|(nx, nz)| (x.clamp(nx as i32 - BOX, nx as i32 + BOX) as i16, z.clamp(nz as i32 - BOX, nz as i32 + BOX) as i16));
            return Err(Blocked::OutOfReach { nearest, gap, fit });
        }
        out.sort_unstable();
        Ok(out)
    }

    /// The standing region the Knight ends up in when he arrives at (`x`, `z`):
    /// that point's, or (in the 32-unit band along a wall, where the probe slides
    /// him out) the nearest one's within [`REACH`].
    pub fn arrival_region(&self, x: i16, z: i16) -> Option<u32> {
        let (x, z) = (x as i32, z as i32);
        if let Some(r) = self.standing(x, z) {
            return Some(r);
        }
        self.nearest_standing(x, z, REACH).and_then(|(nx, nz)| self.standing(nx as i32, nz as i32))
    }

    /// The closest lattice point he can stand on, within `within` units (Chebyshev).
    pub fn nearest_standing(&self, x: i32, z: i32, within: i32) -> Option<(i16, i16)> {
        let mut best: Option<(i32, i32, i32)> = None;
        for (px, pz) in lattice_in(x - within, x + within, z - within, z + within) {
            if self.region[lat(px, pz)] == NONE {
                continue;
            }
            let d = (px - x).abs().max((pz - z).abs());
            let e = (px - x).abs() + (pz - z).abs();
            if best.is_none_or(|(bd, be, _)| (d, e) < (bd, be)) {
                best = Some((d, e, lat(px, pz) as i32));
            }
        }
        best.map(|(_, _, i)| {
            let i = i as usize;
            (((i % L) as i32 * LSTEP - 0x800) as i16, ((i / L) as i32 * LSTEP - 0x800) as i16)
        })
    }

    /// How far a wall cell stands above the lowest floor beside it.
    fn rise(&self, col: u32, row: u32) -> Option<i32> {
        let c = self.comp[row as usize * N + col as usize];
        let h = self.height(col, row) as i32;
        let mut best: Option<i32> = None;
        // The nearest floor cell to it, searched outwards a few cells.
        for r in 1..=8i32 {
            for dr in -r..=r {
                for dc in -r..=r {
                    if dr.abs().max(dc.abs()) != r {
                        continue;
                    }
                    let (cc, rr) = (col as i32 + dc, row as i32 + dr);
                    if !(0..N as i32).contains(&cc) || !(0..N as i32).contains(&rr) {
                        continue;
                    }
                    let k = rr as usize * N + cc as usize;
                    if self.cells[k] == Cell::Floor && self.comp[k] != c {
                        let d = self.heights[k] as i32 - h;
                        best = Some(best.map_or(d, |b: i32| b.min(d)));
                    }
                }
            }
            if best.is_some() {
                return best;
            }
        }
        None
    }

    /// The standing region at a point, or `None` where his centre can't be.
    fn standing(&self, x: i32, z: i32) -> Option<u32> {
        let (px, pz) = (x.clamp(-0x800, 0x7F0), z.clamp(-0x800, 0x7F0));
        if (px, pz) != (x, z) {
            return None;
        }
        // Between lattice points: the limits fall on multiples of 32, so a point
        // stands when the lattice points either side of it do, in one region.
        let (fx, fz) = (floor_to(x), floor_to(z));
        let mut r = NONE;
        for (qx, qz) in [(fx, fz), (ceil_to(x), fz), (fx, ceil_to(z)), (ceil_to(x), ceil_to(z))] {
            if !(-0x800..0x800).contains(&qx) || !(-0x800..0x800).contains(&qz) {
                return None;
            }
            let k = self.region[lat(qx, qz)];
            if k == NONE || (r != NONE && r != k) {
                return None;
            }
            r = k;
        }
        Some(r)
    }

    /// Whether the Knight's centre can be at lattice point (`x`, `z`): his cell is
    /// floor, and the probes [`REACH`] either way on both axes see ground within
    /// [`PROBE`] of it (on the + side a probe landing exactly on a wall's first unit
    /// is where the slide leaves him, so it looks 1 short).
    fn stands(&self, x: i32, z: i32) -> bool {
        let Some(k) = self.index(x, z) else { return false };
        if self.cells[k] != Cell::Floor {
            return false;
        }
        let h = self.heights[k] as i32;
        [(x - REACH, z), (x + REACH - 1, z), (x, z - REACH), (x, z + REACH - 1)].into_iter().all(|(qx, qz)| match self.index(qx, qz) {
            Some(q) => !is_void(self.heights[q]) && (self.heights[q] as i32 - h).abs() <= PROBE,
            None => self.open,
        })
    }

    /// Joins the lattice points he can stand on into regions: neighbours 16 apart
    /// whose cells are a step apart at most.
    fn regions(&self) -> Vec<u32> {
        let mut ok = vec![false; L * L];
        for j in 0..L {
            for i in 0..L {
                ok[j * L + i] = self.stands(i as i32 * LSTEP - 0x800, j as i32 * LSTEP - 0x800);
            }
        }
        let mut region = vec![NONE; L * L];
        let mut next = 0u32;
        let mut stack = Vec::new();
        for start in 0..L * L {
            if !ok[start] || region[start] != NONE {
                continue;
            }
            region[start] = next;
            stack.push(start);
            while let Some(p) = stack.pop() {
                let (i, j) = (p % L, p / L);
                let hp = self.heights[cell_index_of_lattice(i, j)] as i32;
                let mut visit = |q: usize| {
                    if ok[q] && region[q] == NONE && (self.heights[cell_index_of_lattice(q % L, q / L)] as i32 - hp).abs() <= STEP {
                        region[q] = next;
                        stack.push(q);
                    }
                };
                if i > 0 {
                    visit(p - 1);
                }
                if i + 1 < L {
                    visit(p + 1);
                }
                if j > 0 {
                    visit(p - L);
                }
                if j + 1 < L {
                    visit(p + L);
                }
            }
            next += 1;
        }
        region
    }

    fn index(&self, x: i32, z: i32) -> Option<usize> {
        cell_of(x, z).map(|(c, r)| r as usize * N + c as usize)
    }
}

/// Where the doors of `groups` (any pack, every rung) arrive in chunk `chunk`:
/// the entries [`Walk::new`] takes.
pub fn arrivals_into<'a>(chunk: u32, groups: impl IntoIterator<Item = &'a crate::trigger::TriggerGroup>) -> Vec<(i16, i16)> {
    let mut out: Vec<(i16, i16)> = groups
        .into_iter()
        .flat_map(|g| &g.rungs)
        .flat_map(|r| &r.records)
        .filter_map(|r| match *r {
            crate::trigger::TriggerRecord::Door { to, tx, tz, .. } if to as u32 == chunk => Some((tx, tz)),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The cell under a chunk-local point: `(coord + 0x800) >> 5` per axis.
pub fn cell_of(x: i32, z: i32) -> Option<(u32, u32)> {
    ((-0x800..0x800).contains(&x) && (-0x800..0x800).contains(&z)).then(|| (((x + 0x800) >> 5) as u32, ((z + 0x800) >> 5) as u32))
}

fn lat(x: i32, z: i32) -> usize {
    ((z + 0x800) / LSTEP) as usize * L + ((x + 0x800) / LSTEP) as usize
}

fn cell_index_of_lattice(i: usize, j: usize) -> usize {
    (j / 2) * N + i / 2
}

/// Down / up to the lattice (multiples of 16, as −2048 is one).
fn floor_to(v: i32) -> i32 {
    v.div_euclid(LSTEP) * LSTEP
}

fn ceil_to(v: i32) -> i32 {
    -floor_to(-v)
}

/// Lattice points inside [x0, x1] × [z0, z1], within the chunk.
fn lattice_in(x0: i32, x1: i32, z0: i32, z1: i32) -> impl Iterator<Item = (i32, i32)> {
    let lo = |v: i32| ceil_to(v.max(-0x800));
    let hi = |v: i32| floor_to(v.min(0x7F0));
    let (xa, xb, za, zb) = (lo(x0), hi(x1), lo(z0), hi(z1));
    (za..=zb).step_by(LSTEP as usize).flat_map(move |z| (xa..=xb).step_by(LSTEP as usize).map(move |x| (x, z)))
}

/// Non-void cells joined across edges whose step is ≤ [`STEP`].
fn components(h: &[i16]) -> (Vec<u32>, u32) {
    let mut comp = vec![NONE; N * N];
    let mut next = 0u32;
    let mut stack = Vec::new();
    for start in 0..N * N {
        if is_void(h[start]) || comp[start] != NONE {
            continue;
        }
        comp[start] = next;
        stack.push(start);
        while let Some(p) = stack.pop() {
            for q in neighbours(p) {
                if comp[q] == NONE && !is_void(h[q]) && (h[q] as i32 - h[p] as i32).abs() <= STEP {
                    comp[q] = next;
                    stack.push(q);
                }
            }
        }
        next += 1;
    }
    (comp, next)
}

fn neighbours(p: usize) -> impl Iterator<Item = usize> {
    let (c, r) = (p % N, p / N);
    [(c > 0).then(|| p - 1), (c + 1 < N).then(|| p + 1), (r > 0).then(|| p - N), (r + 1 < N).then(|| p + N)].into_iter().flatten()
}

/// A component is a wall when ground borders it and all of that ground is lower
/// (Y down: a larger height): nothing leads up onto it. On a grid chunk a
/// component touching the edge may be reached from the neighbour, and one an
/// entry lands on is reached by that door, so both stay floor.
fn classify(h: &[i16], comp: &[u32], ncomp: u32, open: bool, seeded: &[u32]) -> Vec<Cell> {
    let n = ncomp as usize;
    let (mut up, mut down, mut edge) = (vec![false; n], vec![false; n], vec![false; n]);
    for p in 0..N * N {
        let c = comp[p];
        if c == NONE {
            continue;
        }
        let (col, row) = (p % N, p / N);
        if col == 0 || row == 0 || col == N - 1 || row == N - 1 {
            edge[c as usize] = true;
        }
        for q in neighbours(p) {
            if comp[q] == NONE || comp[q] == c {
                continue;
            }
            if h[q] < h[p] {
                up[c as usize] = true;
            } else {
                down[c as usize] = true;
            }
        }
    }
    (0..N * N)
        .map(|p| match comp[p] {
            NONE => Cell::Void,
            c => {
                let c = c as usize;
                if down[c] && !up[c] && !(open && edge[c]) && !seeded.contains(&(c as u32)) {
                    Cell::Wall
                } else {
                    Cell::Floor
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small house in the middle of an interior: floor −33 inside a ring of
    /// −225 walls, a partition at x cell 70 with a gap of rows 60–62, and void (0)
    /// all around. The same plan as `app/scripts/walk.test.mjs`.
    fn house() -> Vec<i16> {
        let mut h = vec![0i16; N * N];
        for row in 55..=70 {
            for col in 60..=80 {
                let ring = row == 55 || row == 70 || col == 60 || col == 80;
                let partition = col == 70 && !(60..=62).contains(&row);
                h[row * N + col] = if ring || partition { -225 } else { -33 };
            }
        }
        // A bed: a step up onto it, then another.
        h[65 * N + 62] = -65;
        h[65 * N + 63] = -97;
        h
    }

    fn at(col: u32, row: u32) -> (i16, i16) {
        ((col as i32 * 32 - 0x800 + 16) as i16, (row as i32 * 32 - 0x800 + 16) as i16)
    }

    /// @LT-58/2 @LT-58/3: wall tops are classed as walls (the overlay's cells) and refused with a sentence.
    #[test]
    fn walls_are_raised_ground_he_cant_climb_onto() {
        let w = Walk::new(&house(), false, &[]);
        assert_eq!(w.cell(60, 60), Cell::Wall, "the outer ring");
        assert_eq!(w.cell(70, 66), Cell::Wall, "the partition");
        assert_eq!(w.cell(65, 66), Cell::Floor);
        assert_eq!(w.cell(63, 65), Cell::Floor, "the bed is a step from the floor");
        assert_eq!(w.cell(10, 10), Cell::Void);
        assert_eq!(w.walls(), 2 * 21 + 2 * 14 + 11);
        let (x, z) = at(70, 66);
        assert_eq!(w.stand(x, z), Err(Blocked::Wall { col: 70, row: 66, h: -225, rise: Some(192) }));
        assert!(w.stand(x, z).unwrap_err().message(x, z).contains("192 units above the floor"));
        let (x, z) = at(10, 10);
        assert_eq!(w.stand(x, z), Err(Blocked::Void { col: 10, row: 10, h: 0 }));
        assert_eq!(w.stand(at(65, 66).0, at(65, 66).1), Ok(-33));
    }

    #[test]
    fn his_centre_keeps_32_units_from_a_wall() {
        let w = Walk::new(&house(), false, &[]);
        // Row 69 is the last floor row below the ring at row 70 (z 192…223): the
        // probe stops him at z = 192 − 32 = 160 going north.
        let x = at(75, 66).0 as i32;
        assert!(w.can_stand(x, 160));
        assert!(!w.can_stand(x, 161));
        // Row 56 is the first floor row above the ring at row 55 (z −288…−257):
        // going south he stops at −256 + 32 = −224.
        assert!(w.can_stand(x, -224));
        assert!(!w.can_stand(x, -225));
    }

    /// @LT-58/2: a box out of reach is refused with the nearest standing point and a box that fits.
    #[test]
    fn a_box_fires_only_within_reach_of_where_he_can_stand() {
        let w = Walk::new(&house(), false, &[]);
        // A box on the north wall (row 70, z 192…223): the box reaches z 176, he gets to 160.
        let (x, _) = at(75, 70);
        let err = w.reach_box(x, 208).unwrap_err();
        assert_eq!(err, Blocked::OutOfReach { nearest: Some((x, 160)), gap: Some(48), fit: Some((x, 192)) });
        assert!(err.message(x, 208).contains("48 units"), "{}", err.message(x, 208));
        assert!(err.message(x, 208).contains(&format!("Move it to ({x}, 192)")));
        // 16 lower, it reaches.
        assert!(w.reach_box(x, 192).is_ok());
        // Both rooms are one region through the gap in the partition.
        let left = w.reach_box(at(64, 66).0, at(64, 66).1).unwrap();
        let right = w.reach_box(at(76, 66).0, at(76, 66).1).unwrap();
        assert_eq!(left, right);
        assert_eq!(w.arrival_region(at(76, 66).0, at(76, 66).1), Some(right[0]));
    }

    #[test]
    fn a_closed_partition_splits_the_rooms() {
        let mut h = house();
        for row in 60..=62 {
            h[row * N + 70] = -225;
        }
        let w = Walk::new(&h, false, &[]);
        let left = w.reach_box(at(64, 66).0, at(64, 66).1).unwrap();
        let right = w.reach_box(at(76, 66).0, at(76, 66).1).unwrap();
        assert_ne!(left, right);
    }

    /// @LT-58/2: an entry makes the level it lands on floor.
    #[test]
    fn a_level_only_a_door_reaches_is_floor() {
        // A lower floor and, beside it with no stairs, an upper one 128 higher (chunk 333's shape).
        let mut h = vec![0i16; N * N];
        for row in 40..60 {
            for col in 40..60 {
                h[row * N + col] = -33;
            }
            for col in 60..70 {
                h[row * N + col] = -161;
            }
        }
        let (x, z) = at(65, 50);
        let w = Walk::new(&h, false, &[]);
        assert_eq!((w.cell(50, 50), w.cell(65, 50)), (Cell::Floor, Cell::Wall));
        assert!(matches!(w.stand(x, z), Err(Blocked::Wall { rise: Some(128), .. })));
        // A door arriving on it makes it a floor.
        let w = Walk::new(&h, false, &[(x, z)]);
        assert_eq!((w.cell(50, 50), w.cell(65, 50)), (Cell::Floor, Cell::Floor));
        assert_eq!(w.stand(x, z), Ok(-161));
        assert!(w.arrival_region(x, z).is_some());
        assert_ne!(w.arrival_region(x, z), w.arrival_region(at(50, 50).0, at(50, 50).1));
    }

    #[test]
    fn a_grid_chunk_edge_is_open() {
        // A plateau touching the edge of a grid chunk may be reached from the next chunk.
        let mut h = vec![-129i16; N * N];
        for row in 0..10 {
            for col in 0..10 {
                h[row * N + col] = -400;
            }
        }
        assert_eq!(Walk::new(&h, true, &[]).cell(0, 0), Cell::Floor);
        assert_eq!(Walk::new(&h, false, &[]).cell(0, 0), Cell::Wall);
        assert!(Walk::new(&h, true, &[]).can_stand(-0x800, 0));
        assert!(!Walk::new(&h, false, &[]).can_stand(-0x800, 0));
    }

    #[test]
    fn lattice_rounding() {
        assert_eq!(floor_to(-2048), -2048);
        assert_eq!(floor_to(-2047), -2048);
        assert_eq!(floor_to(-2033), -2048);
        assert_eq!(floor_to(-2032), -2032);
        assert_eq!(ceil_to(-2047), -2032);
        assert_eq!(ceil_to(-2032), -2032);
        assert_eq!(ceil_to(5), 16);
        assert_eq!(ceil_to(-5), 0);
        assert_eq!(floor_to(-5), -16);
        assert_eq!(lattice_in(0, 32, 0, 0).count(), 3);
    }
}
