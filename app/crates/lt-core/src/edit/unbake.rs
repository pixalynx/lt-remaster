//! Unbaking a retail chunk (screen 10, `docs/research/12-unbake.md`): the
//! props Tamsoft's level tool merged into the mesh, split back into pieces
//! that move, turn or go, and the chunk baked again around them.
//!
//! **Split.** [`split`] sorts every polygon into terrain ([`super::props::is_terrain`]),
//! ground decals (flat polygons within 4 units of the terrain around them:
//! yards, paths, patches) and props. Props join where they share an *edge*
//! (a vertex alone joins a house to the fence touching it), decals stay out of
//! the join, and pieces with one centre whose footprints overlap merge (a
//! crown on its trunk), as does a piece floating over another (a chimney on
//! its roof). A decal then goes to the one piece whose vertices it
//! shares; one shared by several (a path between two houses) or by none stays
//! with the ground. Pieces that reach the chunk's edge (cliffs, rivers and
//! walls running into the neighbours) are `fixed`.
//!
//! **Cuts.** [`split_cut`] first takes out every polygon a [`Cut`] box holds
//! (its centre inside the box and its height band), each cut a piece of its
//! own: a slice of a bridge fused with the cliff, a wall's broken end.
//!
//! **Rebake.** [`rebake`] takes moved and deleted pieces out, then:
//! - *fills* the cells they leave with no floor (Tamsoft cut the terrain away
//!   under every prop) with ground copied from the nearest flat terrain tile:
//!   whole 128-unit tiles where the hole covers one, else rectangles of 32-unit
//!   cells with the matching part of the tile's texture, each corner at the
//!   terrain's height there (a house on a slope leaves a slope; tiles a cliff
//!   runs through lie flat);
//! - puts the moved pieces back at their new place, turned in quarter turns
//!   about their footprint and raised or lowered to the ground there, in new
//!   512-unit draw groups;
//! - *cuts* terrain the moved piece's own ground covers (no two coplanar
//!   floors, which the PS1's ordering table can't sort);
//! - re-bakes the walk heights of the cells it left, and writes its own walk
//!   cells (pits inside a house, a barrel's top) relative to the new ground;
//! - *welds* the seams where a placed piece meets another: a corner lying on
//!   a neighbour's edge (a T-junction, which the rasterizer leaves pixel cracks
//!   along) splits that edge.
//!
//! A *copy* ([`Move::copy`]) places the piece again and leaves it where it is
//! (Duplicate, and Repeat's row of copies); a move that *holds* keeps its
//! height instead of standing on the ground where it goes (a span over water).
//!
//! [`carry`] gives the same transform to anything placed by position: the
//! doors (type-2 records, drawn as the house's door; ✕ in front of one leads
//! inside), examine spots and chimney smoke of a house. A quarter turn turns a
//! door's facing too, which the game reads to place its ✕ spot. A deleted
//! piece takes them out of the game instead ([`take_pack`], with
//! [`super::remove`]; smoke is hidden too), and putting it back puts them
//! back byte for byte ([`bake_pack`] runs a pack's whole pass).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::npc::NpcPack;
use crate::trigger::Outcome;
use crate::world::Prim;

use super::bake;
use super::chunk::{Chunk, Group, CELLS, MAX_VERTICES};
use super::placement::{Place, PlaceWrite};
use super::props;
use super::{EditError, Result};

/// A polygon: (group, index within the group).
pub type Key = (usize, usize);

/// How far a decal may sit from the terrain around it, and still be ground.
const DECAL_RISE: f32 = 4.0;
/// Pieces whose centres are this close (on both axes) and whose footprints overlap are one.
const SAME_CENTRE: i32 = 16;
/// Draw groups of moved pieces and fills span this many units.
const GROUP_SPAN: i32 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Terrain,
    Decal,
    Prop,
}

/// One movable piece of a chunk.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Piece {
    /// Its polygons, its decals included.
    pub prims: Vec<Key>,
    pub decals: usize,
    /// Vertex bounds, chunk-local `[x, y, z]` (Y down).
    pub min: [i16; 3],
    pub max: [i16; 3],
    /// It reaches the chunk's edge: it continues in the next chunk.
    pub fixed: bool,
    /// The [`Cut`] that made it (its index), or `None`: the split did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<u16>,
}

impl Piece {
    pub fn centre(&self) -> (i32, i32) {
        ((self.min[0] as i32 + self.max[0] as i32) / 2, (self.min[2] as i32 + self.max[2] as i32) / 2)
    }

    fn grow(&mut self, c: &Chunk, k: Key) {
        for v in verts(c, &c.groups[k.0].prims[k.1]) {
            for a in 0..3 {
                self.min[a] = self.min[a].min(v[a]);
                self.max[a] = self.max[a].max(v[a]);
            }
        }
    }

    fn empty() -> Self {
        Piece { prims: Vec::new(), decals: 0, min: [i16::MAX; 3], max: [i16::MIN; 3], fixed: false, cut: None }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Split {
    /// Largest first.
    pub pieces: Vec<Piece>,
    /// Decals no single piece owns (paths between houses): they stay put.
    pub ground: Vec<Key>,
    pub kinds: BTreeMap<String, usize>,
}

fn verts(c: &Chunk, p: &Prim) -> Vec<[i16; 3]> {
    p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]).collect()
}

fn cell_of(x: i32, z: i32) -> Option<usize> {
    let (col, row) = ((x + 2048) >> 5, (z + 2048) >> 5);
    ((0..128).contains(&col) && (0..128).contains(&row)).then(|| (row * 128 + col) as usize)
}

/// Top floor y under each cell (the baker's rule, before the −1).
fn tops(c: &Chunk) -> Vec<Option<f32>> {
    bake::surfaces(c)
        .iter()
        .map(|s| {
            let t = s.iter().filter(|f| f.flat >= bake::FLOOR_FLAT).map(|f| f.y).fold(f32::INFINITY, f32::min);
            t.is_finite().then_some(t)
        })
        .collect()
}

/// `t` grown ring by ring into the cells that have none (the highest neighbour wins).
fn dilate(t: &[Option<f32>]) -> Vec<Option<f32>> {
    let mut cur = t.to_vec();
    for _ in 0..128 {
        let prev = cur.clone();
        let mut changed = false;
        for row in 0..128i32 {
            for col in 0..128i32 {
                let k = (row * 128 + col) as usize;
                if prev[k].is_some() {
                    continue;
                }
                let mut best: Option<f32> = None;
                for (dc, dr) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    let (c2, r2) = (col + dc, row + dr);
                    if (0..128).contains(&c2) && (0..128).contains(&r2) {
                        if let Some(v) = prev[(r2 * 128 + c2) as usize] {
                            // Y is down: the highest ground is the smallest y.
                            best = Some(best.map_or(v, |b: f32| b.min(v)));
                        }
                    }
                }
                if best.is_some() {
                    cur[k] = best;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    cur
}

fn terrain_only(c: &Chunk) -> Chunk {
    let mut t = c.clone();
    for g in t.groups.iter_mut() {
        g.prims.retain(|p| props::is_terrain(c, p));
    }
    t
}

/// The kind of every polygon.
pub fn classify(c: &Chunk) -> HashMap<Key, Kind> {
    let ground = dilate(&tops(&terrain_only(c)));
    let mut out = HashMap::new();
    for (g, grp) in c.groups.iter().enumerate() {
        for (i, p) in grp.prims.iter().enumerate() {
            let k = if props::is_terrain(c, p) {
                Kind::Terrain
            } else {
                let v = verts(c, p);
                let (lo, hi) = (v.iter().map(|q| q[1]).min().unwrap(), v.iter().map(|q| q[1]).max().unwrap());
                let n = v.len() as i32;
                let (cx, cz) = (v.iter().map(|q| q[0] as i32).sum::<i32>() / n, v.iter().map(|q| q[2] as i32).sum::<i32>() / n);
                let on_ground = cell_of(cx, cz).and_then(|k| ground[k]).is_some_and(|t| (lo as f32 - t).abs() <= DECAL_RISE && (hi as f32 - t).abs() <= DECAL_RISE);
                if (hi - lo) as f32 <= DECAL_RISE && on_ground {
                    Kind::Decal
                } else {
                    Kind::Prop
                }
            };
            out.insert((g, i), k);
        }
    }
    out
}

struct Uf(Vec<usize>);

impl Uf {
    fn find(&mut self, i: usize) -> usize {
        let mut r = i;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut j = i;
        while self.0[j] != r {
            let n = self.0[j];
            self.0[j] = r;
            j = n;
        }
        r
    }

    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

/// The edges of a polygon (a quad's diagonal too: strips share it).
fn edges(p: &Prim) -> &'static [(usize, usize)] {
    if p.n == 4 {
        &[(0, 1), (1, 3), (3, 2), (2, 0), (1, 2)]
    } else {
        &[(0, 1), (1, 2), (2, 0)]
    }
}

/// Roof parts (a chimney, a sign on a roof) share no edge with the roof, so the edge join leaves them as pieces
/// of their own, which would float where the roof was when the house moves. A piece whose lowest point is more
/// than [`FLOAT`] above the ground under it goes with the smallest bigger piece that stands over at least half of
/// its cells and reaches as high as its base; repeated, so a part on a part follows too.
fn ride_on_hosts(c: &Chunk, pieces: &mut Vec<Piece>) {
    let ground = dilate(&tops(&terrain_only(c)));
    loop {
        let cells: Vec<BTreeSet<usize>> = pieces.iter().map(|p| cells_under(c, &p.prims)).collect();
        let hosted = (0..pieces.len()).find_map(|n| {
            let p = &pieces[n];
            let g = ground_of(&ground, cells[n].iter().copied())?;
            if g - p.max[1] as i32 <= FLOAT {
                return None;
            }
            let host = (0..pieces.len())
                .filter(|&m| {
                    m != n
                        && pieces[m].prims.len() > p.prims.len()
                        && pieces[m].min[1] <= p.max[1]
                        && cells[n].iter().filter(|k| cells[m].contains(k)).count() * 2 >= cells[n].len()
                })
                .min_by_key(|&m| (cells[m].len(), m))?;
            Some((n, host))
        });
        let Some((n, host)) = hosted else { break };
        let gone = pieces.remove(n);
        let host = if host > n { host - 1 } else { host };
        for k in gone.prims {
            pieces[host].prims.push(k);
            pieces[host].grow(c, k);
        }
    }
}

/// How far above the ground a piece's lowest point may be before it rides on what is under it.
const FLOAT: i32 = 64;

/// A box that cuts a piece of its own out of the chunk (screen 10's Cut): every polygon whose centre lies inside
/// it on x and z (one on its wall doesn't count) and within its height band, terrain included. Cuts take polygons
/// in order: one an earlier cut took stays with that cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cut {
    /// Chunk-local `[x, y, z]` (Y down): the box on x and z, the band on y (inclusive).
    pub min: [i16; 3],
    pub max: [i16; 3],
}

impl Cut {
    /// It holds the polygon with corners `v`.
    pub fn takes(&self, v: &[[i16; 3]]) -> bool {
        let n = v.len() as i32;
        let sum = |a: usize| v.iter().map(|q| q[a] as i32).sum::<i32>();
        let (x, y, z) = (sum(0), sum(1), sum(2));
        let within = |s: i32, a: usize| s > n * self.min[a] as i32 && s < n * self.max[a] as i32;
        within(x, 0) && within(z, 2) && y >= n * self.min[1] as i32 && y <= n * self.max[1] as i32
    }
}

/// The chunk's movable pieces and the ground decals no piece owns.
pub fn split(c: &Chunk) -> Split {
    split_cut(c, &[])
}

/// [`split`], with what `cuts` hold taken out first, one piece per cut that holds any polygon.
pub fn split_cut(c: &Chunk, cuts: &[Cut]) -> Split {
    let kinds = classify(c);
    let mut by_cut: BTreeMap<usize, Vec<Key>> = BTreeMap::new();
    let mut cut_out: HashSet<Key> = HashSet::new();
    if !cuts.is_empty() {
        for (g, grp) in c.groups.iter().enumerate() {
            for (i, p) in grp.prims.iter().enumerate() {
                if let Some(n) = cuts.iter().position(|k| k.takes(&verts(c, p))) {
                    by_cut.entry(n).or_default().push((g, i));
                    cut_out.insert((g, i));
                }
            }
        }
    }
    let props: Vec<Key> = kinds.iter().filter(|(k, kind)| **kind == Kind::Prop && !cut_out.contains(k)).map(|(k, _)| *k).collect::<BTreeSet<_>>().into_iter().collect();
    let mut uf = Uf((0..props.len()).collect());
    let mut by_edge: HashMap<([i16; 3], [i16; 3]), usize> = HashMap::new();
    for (n, &(g, i)) in props.iter().enumerate() {
        let p = &c.groups[g].prims[i];
        let v = verts(c, p);
        for &(a, b) in edges(p) {
            let e = if v[a] < v[b] { (v[a], v[b]) } else { (v[b], v[a]) };
            if e.0 == e.1 {
                continue;
            }
            match by_edge.get(&e) {
                Some(&o) => uf.join(n, o),
                None => {
                    by_edge.insert(e, n);
                }
            }
        }
    }
    let mut roots: BTreeMap<usize, Piece> = BTreeMap::new();
    for (n, &k) in props.iter().enumerate() {
        let r = uf.find(n);
        let o = roots.entry(r).or_insert_with(Piece::empty);
        o.prims.push(k);
        o.grow(c, k);
    }
    let mut pieces: Vec<Piece> = roots.into_values().collect();
    // One centre, overlapping footprints: one piece (a crown on its trunk).
    loop {
        let mut merged = false;
        'outer: for a in 0..pieces.len() {
            for b in a + 1..pieces.len() {
                let (ca, cb) = (pieces[a].centre(), pieces[b].centre());
                let near = (ca.0 - cb.0).abs() <= SAME_CENTRE && (ca.1 - cb.1).abs() <= SAME_CENTRE;
                let (pa, pb) = (&pieces[a], &pieces[b]);
                let overlap = pa.min[0] <= pb.max[0] && pb.min[0] <= pa.max[0] && pa.min[2] <= pb.max[2] && pb.min[2] <= pa.max[2];
                if near && overlap {
                    let gone = pieces.remove(b);
                    for k in gone.prims {
                        pieces[a].prims.push(k);
                        pieces[a].grow(c, k);
                    }
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    ride_on_hosts(c, &mut pieces);
    // Decals: to the one piece whose vertices they share.
    let mut owner: HashMap<[i16; 3], HashSet<usize>> = HashMap::new();
    for (n, o) in pieces.iter().enumerate() {
        for &(g, i) in &o.prims {
            for v in verts(c, &c.groups[g].prims[i]) {
                owner.entry(v).or_default().insert(n);
            }
        }
    }
    let mut ground = Vec::new();
    let decals: BTreeSet<Key> = kinds.iter().filter(|(k, kind)| **kind == Kind::Decal && !cut_out.contains(k)).map(|(k, _)| *k).collect();
    for k in decals {
        let mut who: HashSet<usize> = HashSet::new();
        for v in verts(c, &c.groups[k.0].prims[k.1]) {
            if let Some(s) = owner.get(&v) {
                who.extend(s.iter().copied());
            }
        }
        if who.len() == 1 {
            let n = *who.iter().next().unwrap();
            pieces[n].prims.push(k);
            pieces[n].decals += 1;
            pieces[n].grow(c, k);
        } else {
            ground.push(k);
        }
    }
    for (n, prims) in by_cut {
        let mut p = Piece { cut: Some(n as u16), ..Piece::empty() };
        for k in prims {
            p.prims.push(k);
            p.grow(c, k);
        }
        pieces.push(p);
    }
    for p in pieces.iter_mut() {
        p.fixed = p.min[0] <= -2047 || p.max[0] >= 2047 || p.min[2] <= -2047 || p.max[2] >= 2047;
        p.prims.sort();
    }
    pieces.sort_by(|a, b| b.prims.len().cmp(&a.prims.len()).then(a.min.cmp(&b.min)));
    let mut count = BTreeMap::new();
    for k in kinds.values() {
        *count.entry(format!("{k:?}").to_lowercase()).or_insert(0) += 1;
    }
    Split { pieces, ground, kinds: count }
}

/// Cells whose centre lies under any of `prims` (XZ projection; upright
/// polygons count the cells their corners stand in).
pub fn cells_under(c: &Chunk, prims: &[Key]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    for &(g, i) in prims {
        cells_of(&c.pool, &c.groups[g].prims[i], &mut out);
    }
    out
}

fn cells_of(pool: &[[i16; 3]], p: &Prim, out: &mut BTreeSet<usize>) {
    let v: Vec<[i16; 3]> = p.idx[..p.n as usize].iter().map(|&i| pool[i as usize]).collect();
    let tris: &[[usize; 3]] = if p.n == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] };
    for t in tris {
        let (a, b, d) = (v[t[0]], v[t[1]], v[t[2]]);
        let area = (b[0] as f32 - a[0] as f32) * (d[2] as f32 - a[2] as f32) - (d[0] as f32 - a[0] as f32) * (b[2] as f32 - a[2] as f32);
        if area.abs() < 1.0 {
            out.extend([a, b, d].iter().filter_map(|q| cell_of(q[0] as i32, q[2] as i32)));
            continue;
        }
        let (x0, x1) = (a[0].min(b[0]).min(d[0]) as i32, a[0].max(b[0]).max(d[0]) as i32);
        let (z0, z1) = (a[2].min(b[2]).min(d[2]) as i32, a[2].max(b[2]).max(d[2]) as i32);
        for row in ((z0 + 2048) >> 5).max(0)..=((z1 + 2048) >> 5).min(127) {
            for col in ((x0 + 2048) >> 5).max(0)..=((x1 + 2048) >> 5).min(127) {
                let (px, pz) = bake::cell_centre(col as usize, row as usize);
                let e = |p: [i16; 3], q: [i16; 3]| (q[0] as f32 - p[0] as f32) * (pz - p[2] as f32) - (q[2] as f32 - p[2] as f32) * (px - p[0] as f32);
                let (e0, e1, e2) = (e(a, b), e(b, d), e(d, a));
                if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0) {
                    out.insert((row * 128 + col) as usize);
                }
            }
        }
    }
}

/// A piece's new place: whole cells, quarter turns (counter-clockwise seen
/// from above with north, +z, up, as screen 10's top view draws it; about its
/// footprint), or gone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    pub piece: usize,
    /// Cells east (+x) and north (+z).
    pub dc: i16,
    pub dr: i16,
    pub turns: u8,
    pub delete: bool,
    /// A copy goes there; the piece stays (never with `delete`).
    pub copy: bool,
    /// It keeps its height rather than standing on the ground there.
    pub hold: bool,
}

/// What a rebake did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rebaked {
    /// Cells filled with ground, and the polygons that took.
    pub filled_cells: usize,
    pub fill_polygons: usize,
    /// Terrain polygons a moved piece's own ground covered.
    pub cut_polygons: usize,
    pub moved_polygons: usize,
    pub copied_polygons: usize,
    pub deleted_polygons: usize,
    /// Polygons split where a placed piece's corner lay on another piece's edge.
    pub welded_polygons: usize,
    /// Walk cells re-baked where pieces left, and written where they went.
    pub cleared_cells: usize,
    pub written_cells: usize,
    /// Units each moved piece rose (−) or sank (+) to sit on its new ground.
    pub lifts: Vec<i16>,
}

/// The transform of a move, for vertices and anything placed by position.
#[derive(Debug, Clone, Copy)]
pub struct Transform {
    /// Footprint min corner (cell-aligned) and size in units.
    x0: i32,
    z0: i32,
    w: i32,
    h: i32,
    turns: u8,
    dx: i32,
    dz: i32,
    pub dy: i32,
}

impl Transform {
    pub fn point(&self, x: i32, z: i32) -> (i32, i32) {
        let (lx, lz) = (x - self.x0, z - self.z0);
        let (rx, rz) = match self.turns % 4 {
            0 => (lx, lz),
            1 => (self.h - lz, lx),
            2 => (self.w - lx, self.h - lz),
            _ => (lz, self.w - lx),
        };
        (self.x0 + self.dx + rx, self.z0 + self.dz + rz)
    }

    /// A facing (0–4095) after the turn. A quarter turn counter-clockwise
    /// (north, +z, up) takes +x to +z; a facing is 0 toward +z and 1024
    /// toward +x (screen 10's `facing.ts`), so it loses 1024 per turn.
    pub fn dir(&self, dir: u16) -> u16 {
        (dir as u32 + 4096 - 1024 * (self.turns as u32 % 4)) as u16 & 0xFFF
    }

    fn cell(&self, k: usize) -> Option<usize> {
        let (col, row) = ((k % 128) as i32, (k / 128) as i32);
        let (x, z) = (col * 32 - 2048, row * 32 - 2048);
        // Turn the cell's square, then take its min corner.
        let (a, b) = (self.point(x, z), self.point(x + 32, z + 32));
        cell_of(a.0.min(b.0), a.1.min(b.1))
    }
}

/// The footprint a piece turns about: its bounds, out to whole cells.
pub fn transform(p: &Piece, m: &Move, dy: i32) -> Transform {
    let x0 = (p.min[0] as i32 + 2048).div_euclid(32) * 32 - 2048;
    let z0 = (p.min[2] as i32 + 2048).div_euclid(32) * 32 - 2048;
    let x1 = (p.max[0] as i32 + 2048 + 31).div_euclid(32) * 32 - 2048;
    let z1 = (p.max[2] as i32 + 2048 + 31).div_euclid(32) * 32 - 2048;
    let (w, h) = (x1 - x0, z1 - z0);
    // Turned a quarter, a w × h footprint becomes h × w about the same corner.
    Transform { x0, z0, w, h, turns: m.turns % 4, dx: m.dc as i32 * 32, dz: m.dr as i32 * 32, dy }
}

/// The cells of a piece's footprint (its bounds out to whole cells): its own walk cells. A wall on the
/// footprint's edge stands in the next cell too ([`cells_under`]), which isn't the piece's.
fn footprint(p: &Piece) -> BTreeSet<usize> {
    let t = transform(p, &Move::default(), 0);
    let mut out = BTreeSet::new();
    for z in (t.z0..t.z0 + t.h.max(32)).step_by(32) {
        for x in (t.x0..t.x0 + t.w.max(32)).step_by(32) {
            out.extend(cell_of(x, z));
        }
    }
    out
}

/// The moved footprint lies within the chunk.
fn inside_chunk(p: &Piece, m: &Move) -> bool {
    let t = transform(p, m, 0);
    let (a, b) = (t.point(t.x0, t.z0), t.point(t.x0 + t.w, t.z0 + t.h));
    a.0.min(b.0) >= -2048 && a.0.max(b.0) <= 2048 && a.1.min(b.1) >= -2048 && a.1.max(b.1) <= 2048
}

/// Moves `(x, z)` with the piece when it lies within `margin` of its bounds.
pub fn carry(split: &Split, m: &Move, lift: i16, x: i32, z: i32, margin: i32) -> Option<(i32, i32)> {
    let p = split.pieces.get(m.piece)?;
    let inside = x >= p.min[0] as i32 - margin && x <= p.max[0] as i32 + margin && z >= p.min[2] as i32 - margin && z <= p.max[2] as i32 + margin;
    (inside && !m.delete && !m.copy).then(|| transform(p, m, lift as i32).point(x, z))
}

/// A flat 128-unit terrain tile a fill copies: its quad and corners.
#[derive(Clone, Copy)]
struct Donor {
    prim: Prim,
    /// Corner k's place in the tile: (0 or 1, 0 or 1) on x and z.
    at: [(u8, u8); 4],
}

impl Donor {
    /// Signed XZ area of the polygon its strip draws (> 0 or < 0: the front face's winding).
    fn facing(&self) -> f32 {
        let corner = |k: usize| [self.at[k].0 as f32, self.at[k].1 as f32];
        area(&[corner(0), corner(1), corner(3), corner(2)])
    }

    fn key(&self) -> (u16, u16, [[u8; 2]; 4], u8) {
        (self.prim.tpage, self.prim.clut, self.prim.uv, self.prim.list)
    }

    /// uv and colour at (fx, fz) in 0..=1 across the tile.
    fn sample(&self, fx: f32, fz: f32) -> ([u8; 2], [u8; 3]) {
        let mut corner = [0usize; 4];
        for k in 0..4 {
            let (a, b) = self.at[k];
            corner[(b as usize) * 2 + a as usize] = k;
        }
        let w = [(1.0 - fx) * (1.0 - fz), fx * (1.0 - fz), (1.0 - fx) * fz, fx * fz];
        let (mut uv, mut rgb) = ([0f32; 2], [0f32; 3]);
        for (j, &k) in corner.iter().enumerate() {
            for a in 0..2 {
                uv[a] += w[j] * self.prim.uv[k][a] as f32;
            }
            for a in 0..3 {
                rgb[a] += w[j] * self.prim.rgb[k][a] as f32;
            }
        }
        (uv.map(|v| v.round().clamp(0.0, 255.0) as u8), rgb.map(|v| v.round().clamp(0.0, 255.0) as u8))
    }
}

/// `poly` (convex, XZ) clipped to the square `[x0, x0 + s] × [z0, z0 + s]`.
fn clip(poly: &[[f32; 2]], x0: f32, z0: f32, s: f32) -> Vec<[f32; 2]> {
    let mut out = poly.to_vec();
    for (axis, bound, low) in [(0, x0, true), (0, x0 + s, false), (1, z0, true), (1, z0 + s, false)] {
        let input = std::mem::take(&mut out);
        let inside = |p: &[f32; 2]| if low { p[axis] >= bound - 1e-3 } else { p[axis] <= bound + 1e-3 };
        for i in 0..input.len() {
            let (a, b) = (input[i], input[(i + 1) % input.len()]);
            if inside(&a) {
                out.push(a);
            }
            if inside(&a) != inside(&b) {
                let t = (bound - a[axis]) / (b[axis] - a[axis]);
                out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
            }
        }
    }
    out.dedup_by(|a, b| (a[0] - b[0]).abs() < 0.5 && (a[1] - b[1]).abs() < 0.5);
    while out.len() > 1 && (out[0][0] - out[out.len() - 1][0]).abs() < 0.5 && (out[0][1] - out[out.len() - 1][1]).abs() < 0.5 {
        out.pop();
    }
    out
}

/// Signed area of an XZ polygon.
fn area(p: &[[f32; 2]]) -> f32 {
    (0..p.len()).map(|i| {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        a[0] * b[1] - b[0] * a[1]
    }).sum::<f32>() / 2.0
}

/// A convex polygon as quads and triangles in polygon order, wound with
/// the sign `positive` (the terrain's front face).
fn fan(p: &[[f32; 2]], positive: bool) -> Vec<Vec<[f32; 2]>> {
    let mut p = p.to_vec();
    if (area(&p) > 0.0) != positive {
        p.reverse();
    }
    if p.len() <= 4 {
        return vec![p];
    }
    (1..p.len() - 1).map(|i| vec![p[0], p[i], p[i + 1]]).collect()
}

/// Flat terrain quads that are exactly one 128-unit tile, by tile (col, row of 32).
fn donors(c: &Chunk) -> HashMap<(i32, i32), Donor> {
    let mut out = HashMap::new();
    for g in &c.groups {
        for p in &g.prims {
            if p.n != 4 || !p.textured() || !props::is_terrain(c, p) {
                continue;
            }
            let v = verts(c, p);
            if v.iter().any(|q| q[1] != v[0][1]) {
                continue;
            }
            let (x0, z0) = (v.iter().map(|q| q[0]).min().unwrap() as i32, v.iter().map(|q| q[2]).min().unwrap() as i32);
            let (x1, z1) = (v.iter().map(|q| q[0]).max().unwrap() as i32, v.iter().map(|q| q[2]).max().unwrap() as i32);
            if x1 - x0 != 128 || z1 - z0 != 128 {
                continue;
            }
            let at = [0, 1, 2, 3].map(|k| (((v[k][0] as i32 - x0) / 128) as u8, ((v[k][2] as i32 - z0) / 128) as u8));
            let distinct: HashSet<(u8, u8)> = at.iter().copied().collect();
            if distinct.len() == 4 {
                out.insert(((x0 + 2048) / 128, (z0 + 2048) / 128), Donor { prim: *p, at });
            }
        }
    }
    out
}

/// The donor a hole tile copies: the commonest look among the nearest ring of tiles that has any.
fn donor_for(donors: &HashMap<(i32, i32), Donor>, tc: i32, tr: i32) -> Option<Donor> {
    for ring in 0i32..8 {
        let mut seen: HashMap<(u16, u16, [[u8; 2]; 4], u8), (usize, Donor)> = HashMap::new();
        for dr in -ring..=ring {
            for dc in -ring..=ring {
                if dr.abs().max(dc.abs()) != ring {
                    continue;
                }
                if let Some(d) = donors.get(&(tc + dc, tr + dr)) {
                    seen.entry(d.key()).or_insert((0, *d)).0 += 1;
                }
            }
        }
        if let Some((_, (_, d))) = seen.into_iter().max_by_key(|(k, (n, _))| (*n, std::cmp::Reverse(*k))) {
            return Some(d);
        }
    }
    None
}

/// The terrain's vertex colours on the 128-unit lattice (33 × 33 points),
/// grown into the points under props that have none. Retail ground is tinted
/// by a smooth field (no shadows under props: research 12 §3), so a fill
/// takes its corners' colours from it and blends in.
fn shade(c: &Chunk) -> Vec<[f32; 3]> {
    let mut sum = vec![([0f32; 3], 0u32); 33 * 33];
    for g in &c.groups {
        for p in &g.prims {
            if !props::is_terrain(c, p) {
                continue;
            }
            for k in 0..p.n as usize {
                let v = c.pool[p.idx[k] as usize];
                let (i, j) = ((v[0] as i32 + 2048) / 128, (v[2] as i32 + 2048) / 128);
                if (0..33).contains(&i) && (0..33).contains(&j) {
                    let e = &mut sum[(j * 33 + i) as usize];
                    for a in 0..3 {
                        e.0[a] += p.rgb[k][a] as f32;
                    }
                    e.1 += 1;
                }
            }
        }
    }
    let mut grid: Vec<Option<[f32; 3]>> = sum.iter().map(|(s, n)| (*n > 0).then(|| s.map(|v| v / *n as f32))).collect();
    for _ in 0..33 {
        let prev = grid.clone();
        let mut changed = false;
        for j in 0..33i32 {
            for i in 0..33i32 {
                if prev[(j * 33 + i) as usize].is_some() {
                    continue;
                }
                let near: Vec<[f32; 3]> = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .filter_map(|(di, dj)| {
                        let (a, b) = (i + di, j + dj);
                        ((0..33).contains(&a) && (0..33).contains(&b)).then(|| prev[(b * 33 + a) as usize]).flatten()
                    })
                    .collect();
                if !near.is_empty() {
                    let n = near.len() as f32;
                    grid[(j * 33 + i) as usize] = Some([0, 1, 2].map(|a| near.iter().map(|c| c[a]).sum::<f32>() / n));
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    grid.into_iter().map(|g| g.unwrap_or([128.0; 3])).collect()
}

/// The terrain's height on the 128-unit lattice (33 × 33 points): at each point the commonest y of the terrain
/// vertices there (the higher on a tie). Points with none (Tamsoft cut the terrain away under props) are filled
/// smoothly from their neighbours, a harmonic fill, so the slope a house stood on comes back as that slope and a
/// fill meets the terrain around it edge to edge.
fn height_lattice(c: &Chunk) -> Vec<f32> {
    let mut seen: Vec<HashMap<i16, u32>> = vec![HashMap::new(); 33 * 33];
    for g in &c.groups {
        for p in &g.prims {
            if !props::is_terrain(c, p) {
                continue;
            }
            for &i in &p.idx[..p.n as usize] {
                let v = c.pool[i as usize];
                let (x, z) = (v[0] as i32 + 2048, v[2] as i32 + 2048);
                if x % 128 == 0 && z % 128 == 0 && (0..=4096).contains(&x) && (0..=4096).contains(&z) {
                    *seen[(z / 128 * 33 + x / 128) as usize].entry(v[1]).or_default() += 1;
                }
            }
        }
    }
    let known: Vec<Option<f32>> = seen.iter().map(|m| m.iter().max_by_key(|(y, n)| (**n, -(**y as i32))).map(|(y, _)| *y as f32)).collect();
    // Grown in ring by ring, then smoothed: each unknown point the mean of its neighbours.
    let mut grid = known.clone();
    for _ in 0..33 {
        let prev = grid.clone();
        let mut changed = false;
        for j in 0..33i32 {
            for i in 0..33i32 {
                if prev[(j * 33 + i) as usize].is_some() {
                    continue;
                }
                let near: Vec<f32> = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .filter_map(|(di, dj)| {
                        let (a, b) = (i + di, j + dj);
                        ((0..33).contains(&a) && (0..33).contains(&b)).then(|| prev[(b * 33 + a) as usize]).flatten()
                    })
                    .collect();
                if !near.is_empty() {
                    grid[(j * 33 + i) as usize] = Some(near.iter().sum::<f32>() / near.len() as f32);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut h: Vec<f32> = grid.into_iter().map(|g| g.unwrap_or(-128.0)).collect();
    for _ in 0..200 {
        let prev = h.clone();
        for j in 0..33i32 {
            for i in 0..33i32 {
                let k = (j * 33 + i) as usize;
                if known[k].is_some() {
                    continue;
                }
                let near: Vec<f32> = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .filter(|(di, dj)| (0..33).contains(&(i + di)) && (0..33).contains(&(j + dj)))
                    .map(|(di, dj)| prev[((j + dj) * 33 + i + di) as usize])
                    .collect();
                h[k] = near.iter().sum::<f32>() / near.len() as f32;
            }
        }
    }
    h
}

/// Tiles whose lattice corners differ by more than this aren't a slope to follow (a cliff runs through them):
/// a fill there lies flat, or it would be too steep to walk on.
const GENTLE: f32 = 64.0;

/// The lattice corners of tile (tc, tr) are a gentle slope.
fn gentle(lattice: &[f32], tc: i32, tr: i32) -> bool {
    let (tc, tr) = (tc.clamp(0, 31), tr.clamp(0, 31));
    let c = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(a, b)| lattice[((tr + b) * 33 + tc + a) as usize]);
    c.iter().cloned().fold(f32::MIN, f32::max) - c.iter().cloned().fold(f32::MAX, f32::min) <= GENTLE
}

/// The ground height (PSX Y) at chunk-local (x, z), bilinear between lattice points.
fn height_at(lattice: &[f32], x: i32, z: i32) -> i32 {
    let (fx, fz) = ((x + 2048) as f32 / 128.0, (z + 2048) as f32 / 128.0);
    let (i, j) = ((fx.floor() as i32).clamp(0, 31), (fz.floor() as i32).clamp(0, 31));
    let (u, v) = (fx - i as f32, fz - j as f32);
    let at = |a: i32, b: i32| lattice[(b * 33 + a) as usize];
    ((1.0 - u) * (1.0 - v) * at(i, j) + u * (1.0 - v) * at(i + 1, j) + (1.0 - u) * v * at(i, j + 1) + u * v * at(i + 1, j + 1)).round() as i32
}

/// The shade at chunk-local (x, z), bilinear between lattice points.
fn shade_at(grid: &[[f32; 3]], x: i32, z: i32) -> [u8; 3] {
    let (fx, fz) = ((x + 2048) as f32 / 128.0, (z + 2048) as f32 / 128.0);
    let (i, j) = ((fx.floor() as i32).clamp(0, 31), (fz.floor() as i32).clamp(0, 31));
    let (u, v) = (fx - i as f32, fz - j as f32);
    let at = |a: i32, b: i32| grid[(b * 33 + a) as usize];
    let (p00, p10, p01, p11) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
    [0, 1, 2].map(|a| ((1.0 - u) * (1.0 - v) * p00[a] + u * (1.0 - v) * p10[a] + (1.0 - u) * v * p01[a] + u * v * p11[a]).round().clamp(0.0, 255.0) as u8)
}

/// New draw groups for added polygons, one per 512-unit square.
#[derive(Default)]
struct Buckets(BTreeMap<(i32, i32), Vec<Prim>>);

impl Buckets {
    fn add(&mut self, pool: &[[i16; 3]], q: Prim) {
        let n = q.n as usize;
        let cx = q.idx[..n].iter().map(|&i| pool[i as usize][0] as i32).sum::<i32>() / n as i32;
        let cz = q.idx[..n].iter().map(|&i| pool[i as usize][2] as i32).sum::<i32>() / n as i32;
        self.0.entry(((cx + 2048).div_euclid(GROUP_SPAN), (cz + 2048).div_euclid(GROUP_SPAN))).or_default().push(q);
    }
}

fn push_vertex(pool: &mut Vec<[i16; 3]>, index: &mut HashMap<[i16; 3], u16>, v: [i32; 3]) -> Result<u16> {
    let v = v.map(|a| a.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
    if let Some(&i) = index.get(&v) {
        return Ok(i);
    }
    if pool.len() >= MAX_VERTICES {
        return Err(EditError::new("CHUNK_VERTICES", format!("The chunk would hold more than {MAX_VERTICES} vertices.")));
    }
    pool.push(v);
    let i = (pool.len() - 1) as u16;
    index.insert(v, i);
    Ok(i)
}

/// The most common ground y (PSX Y) of `cells` in `ground`.
fn ground_of(ground: &[Option<f32>], cells: impl IntoIterator<Item = usize>) -> Option<i32> {
    let mut count: HashMap<i32, usize> = HashMap::new();
    for k in cells {
        if let Some(y) = ground[k] {
            *count.entry(y.round() as i32).or_default() += 1;
        }
    }
    count.into_iter().max_by_key(|&(y, n)| (n, -y)).map(|(y, _)| y)
}

/// The ring of cells just outside `cells`.
fn ring(cells: &BTreeSet<usize>) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    for &k in cells {
        let (col, row) = ((k % 128) as i32, (k / 128) as i32);
        for (dc, dr) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
            let (c2, r2) = (col + dc, row + dr);
            if (0..128).contains(&c2) && (0..128).contains(&r2) {
                let n = (r2 * 128 + c2) as usize;
                if !cells.contains(&n) {
                    out.insert(n);
                }
            }
        }
    }
    out
}

/// Cells `(i, j)` of one tile (0..4 each) as few rectangles `(i, j, w, h)`:
/// the first cell left (row by row), as wide as it runs, then as tall as that
/// whole run goes.
fn rectangles(cells: impl IntoIterator<Item = (i32, i32)>) -> Vec<(i32, i32, i32, i32)> {
    let mut left = [[false; 4]; 4];
    for (i, j) in cells {
        if (0..4).contains(&i) && (0..4).contains(&j) {
            left[j as usize][i as usize] = true;
        }
    }
    let mut out = Vec::new();
    for j in 0..4 {
        for i in 0..4 {
            if !left[j][i] {
                continue;
            }
            let w = (i..4).take_while(|&x| left[j][x]).count();
            let h = (j..4).take_while(|&y| (i..i + w).all(|x| left[y][x])).count();
            for row in left.iter_mut().skip(j).take(h) {
                row[i..i + w].iter_mut().for_each(|c| *c = false);
            }
            out.push((i as i32, j as i32, w as i32, h as i32));
        }
    }
    out
}

/// Twice the XZ area a polygon with corners `v` covers (an upright wall's is 0).
fn flat_xz_area(v: &[[i16; 3]]) -> f32 {
    let tri = |a: [i16; 3], b: [i16; 3], d: [i16; 3]| ((b[0] as f32 - a[0] as f32) * (d[2] as f32 - a[2] as f32) - (d[0] as f32 - a[0] as f32) * (b[2] as f32 - a[2] as f32)).abs();
    if v.len() == 4 {
        tri(v[0], v[1], v[2]) + tri(v[1], v[3], v[2])
    } else {
        tri(v[0], v[1], v[2])
    }
}

/// How far off an edge's line (units) a corner may lie and still be on it.
const WELD_SLACK: i64 = 1;

/// Where `v` lies on the edge `a`–`b` (0–1, ends excluded), or `None`: it isn't on it.
fn on_edge(a: [i16; 3], b: [i16; 3], v: [i16; 3]) -> Option<f32> {
    let d = [0, 1, 2].map(|k| b[k] as i64 - a[k] as i64);
    let w = [0, 1, 2].map(|k| v[k] as i64 - a[k] as i64);
    let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let dot = d[0] * w[0] + d[1] * w[1] + d[2] * w[2];
    // Not at (or within 2 units of) either end: a cut there would leave a sliver.
    let near_end = |e: [i16; 3]| (0..3).map(|k| (v[k] as i64 - e[k] as i64).pow(2)).sum::<i64>() < 4;
    if dd == 0 || dot <= 0 || dot >= dd || near_end(a) || near_end(b) {
        return None;
    }
    let x = [d[1] * w[2] - d[2] * w[1], d[2] * w[0] - d[0] * w[2], d[0] * w[1] - d[1] * w[0]];
    // |d × w| is |d| times the distance off the line.
    (x[0] * x[0] + x[1] * x[1] + x[2] * x[2] <= dd * WELD_SLACK * WELD_SLACK).then(|| dot as f32 / dd as f32)
}

/// A corner of a polygon being split: its vertex, texture point and colour.
#[derive(Clone, Copy)]
struct Corner {
    idx: u16,
    uv: [u8; 2],
    rgb: [u8; 3],
}

impl Corner {
    /// The corner at `idx`, `t` of the way from `self` to `b`.
    fn toward(self, b: Corner, t: f32, idx: u16) -> Corner {
        let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
        Corner { idx, uv: [l(self.uv[0], b.uv[0]), l(self.uv[1], b.uv[1])], rgb: [0, 1, 2].map(|k| l(self.rgb[k], b.rgb[k])) }
    }
}

/// Triangle `tri` cut through every one of `points` lying on its edges: each cut joins the point to the corner
/// across, so the parts keep the triangle's winding. A point cuts once (the parts look for the others).
fn split_tri(pool: &[[i16; 3]], tri: [Corner; 3], points: &[u16], out: &mut Vec<[Corner; 3]>) {
    for e in 0..3 {
        let (a, b, o) = (tri[e], tri[(e + 1) % 3], tri[(e + 2) % 3]);
        for (n, &p) in points.iter().enumerate() {
            if let Some(t) = on_edge(pool[a.idx as usize], pool[b.idx as usize], pool[p as usize]) {
                let m = a.toward(b, t, p);
                let rest: Vec<u16> = points.iter().enumerate().filter(|&(k, _)| k != n).map(|(_, &q)| q).collect();
                split_tri(pool, [a, m, o], &rest, out);
                split_tri(pool, [m, b, o], &rest, out);
                return;
            }
        }
    }
    out.push(tri);
}

/// `q` as triangles meeting `points` (corners of other pieces on its edges): a quad first as the two triangles
/// the GPU draws it as (0-1-2 and 1-3-2, each mapped affinely), so the texture runs on as it was drawn.
fn split_prim(pool: &[[i16; 3]], q: &Prim, points: &[u16]) -> Vec<Prim> {
    let corner = |k: usize| Corner { idx: q.idx[k], uv: q.uv[k], rgb: q.rgb[k] };
    let halves: &[[usize; 3]] = if q.n == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] };
    let mut tris = Vec::new();
    for h in halves {
        split_tri(pool, h.map(corner), points, &mut tris);
    }
    tris.into_iter()
        .map(|t| {
            let mut p = *q;
            p.n = 3;
            // F4 FT4 G4 GT4 (2 3 6 7) draw as F3 FT3 G3 GT3 (0 1 4 5).
            p.list = if q.n == 4 { q.list - 2 } else { q.list };
            p.idx = [t[0].idx, t[1].idx, t[2].idx, t[0].idx];
            p.uv = [t[0].uv, t[1].uv, t[2].uv, t[0].uv];
            p.rgb = [t[0].rgb, t[1].rgb, t[2].rgb, t[0].rgb];
            p
        })
        .collect()
}

/// Welds the seams copies make (crack-free joins): wherever a corner of one piece lies on the edge of another's
/// polygon (a T-junction, along which the rasterizer leaves pixel cracks), and one of the two is a copy or both
/// were just placed (`placed`, by move: `placed_by`, 1-based; `copies`: which moves are copies), that polygon
/// becomes triangles meeting the corner. Joins within one piece stay as Tamsoft made them, a piece moved on its
/// own meets the chunk as before (no polygons added), and terrain isn't split. Returns how many polygons were.
fn weld(out: &mut Chunk, placed: &mut Vec<Prim>, placed_by: &[u32], copies: &[bool]) -> usize {
    let copy = |o: u32| o > 0 && copies[o as usize - 1];
    let joins = |o: u32, w: u32| w != o && (copy(o) || copy(w) || (o > 0 && w > 0));
    if placed.is_empty() {
        return 0;
    }
    let corners = |p: &Prim| p.idx[..p.n as usize].to_vec();
    let (mut lo, mut hi) = ([i16::MAX; 3], [i16::MIN; 3]);
    for p in placed.iter() {
        for i in corners(p) {
            let v = out.pool[i as usize];
            for a in 0..3 {
                lo[a] = lo[a].min(v[a]);
                hi[a] = hi[a].max(v[a]);
            }
        }
    }
    let near = |p: &Prim| {
        let v: Vec<[i16; 3]> = corners(p).iter().map(|&i| out.pool[i as usize]).collect();
        (0..3).all(|a| v.iter().map(|q| q[a]).max().unwrap() as i32 >= lo[a] as i32 - 2 && v.iter().map(|q| q[a]).min().unwrap() as i32 <= hi[a] as i32 + 2)
    };
    // Candidates: (its key, or None for placed[j]; j; the polygon; the move that placed it, 0: none).
    let mut cands: Vec<(Option<Key>, usize, Prim, u32)> = Vec::new();
    for (g, grp) in out.groups.iter().enumerate() {
        for (i, p) in grp.prims.iter().enumerate() {
            if !props::is_terrain(out, p) && near(p) {
                cands.push((Some((g, i)), 0, *p, 0));
            }
        }
    }
    for (j, p) in placed.iter().enumerate() {
        cands.push((None, j, *p, placed_by[j]));
    }
    let mut origins: HashMap<u16, Vec<u32>> = HashMap::new();
    for (_, _, p, o) in &cands {
        for i in corners(p) {
            let e = origins.entry(i).or_default();
            if !e.contains(o) {
                e.push(*o);
            }
        }
    }
    let verts: Vec<(u16, [i16; 3])> = origins.keys().map(|&i| (i, out.pool[i as usize])).collect();
    let mut split = 0;
    let mut add: Vec<(usize, Prim)> = Vec::new();
    for (at, j, p, o) in cands {
        let own = corners(&p);
        let ring: &[usize] = if p.n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
        let mut points: Vec<u16> = Vec::new();
        for e in 0..ring.len() {
            let (a, b) = (out.pool[p.idx[ring[e]] as usize], out.pool[p.idx[ring[(e + 1) % ring.len()]] as usize]);
            let (x0, x1) = (a[0].min(b[0]) - 1, a[0].max(b[0]) + 1);
            let (z0, z1) = (a[2].min(b[2]) - 1, a[2].max(b[2]) + 1);
            for &(i, v) in &verts {
                // Another piece's corner (one side or the other just placed), on this edge.
                if v[0] < x0 || v[0] > x1 || v[2] < z0 || v[2] > z1 || own.contains(&i) || points.contains(&i) {
                    continue;
                }
                if origins[&i].iter().any(|&w| joins(o, w)) && on_edge(a, b, v).is_some() {
                    points.push(i);
                }
            }
        }
        if points.is_empty() {
            continue;
        }
        let mut parts = split_prim(&out.pool, &p, &points).into_iter();
        let first = parts.next().expect("a polygon splits into at least one triangle");
        split += 1;
        match at {
            Some((g, i)) => {
                out.groups[g].prims[i] = first;
                add.extend(parts.map(|q| (g, q)));
            }
            None => {
                placed[j] = first;
                add.extend(parts.map(|q| (usize::MAX, q)));
            }
        }
    }
    for (g, q) in add {
        if g == usize::MAX {
            placed.push(q);
        } else {
            out.groups[g].prims.push(q);
        }
    }
    split
}

/// Drops vertices no polygon uses and renumbers the rest.
fn compact(c: &mut Chunk) {
    let mut used = vec![false; c.pool.len()];
    for g in &c.groups {
        for p in &g.prims {
            for &i in &p.idx[..p.n as usize] {
                used[i as usize] = true;
            }
        }
    }
    let mut map = vec![u16::MAX; c.pool.len()];
    let mut pool = Vec::new();
    for (i, v) in c.pool.iter().enumerate() {
        if used[i] {
            map[i] = pool.len() as u16;
            pool.push(*v);
        }
    }
    for g in c.groups.iter_mut() {
        for p in g.prims.iter_mut() {
            let n = p.n as usize;
            for i in p.idx[..n].iter_mut() {
                *i = map[*i as usize];
            }
            // Unused corners of a triangle point at its first vertex.
            for k in n..4 {
                p.idx[k] = p.idx[0];
            }
        }
    }
    c.pool = pool;
}

/// `c` with `moves` applied to the pieces of `split` (made from `c`).
pub fn rebake(c: &Chunk, split: &Split, moves: &[Move]) -> Result<(Chunk, Rebaked)> {
    let mut rep = Rebaked::default();
    for m in moves {
        let p = split.pieces.get(m.piece).ok_or_else(|| EditError::new("UNBAKE_PIECE", format!("There is no piece {}.", m.piece)))?;
        // A copy of a fixed piece is new geometry; the piece itself stays.
        if p.fixed && !m.delete && !m.copy && (m.dc, m.dr, m.turns % 4) != (0, 0, 0) {
            return Err(EditError::new("UNBAKE_FIXED", format!("Piece {} runs into the next chunk; it can't move.", m.piece)));
        }
        if m.copy && m.delete {
            return Err(EditError::new("UNBAKE_COPY", format!("A copy of piece {} can't be a delete.", m.piece)));
        }
        if !m.delete && !inside_chunk(p, m) {
            return Err(EditError::new("UNBAKE_OUTSIDE", format!("Piece {} would leave the chunk; keep it inside.", m.piece)));
        }
    }
    // Moves and deletes take their piece away; copies leave it.
    let away = || moves.iter().filter(|m| !m.copy);
    let gone: HashSet<Key> = away().flat_map(|m| split.pieces[m.piece].prims.iter().copied()).collect();
    let left: BTreeSet<usize> = away().flat_map(|m| cells_under(c, &split.pieces[m.piece].prims)).collect();
    let vacated: BTreeSet<usize> = away().flat_map(|m| footprint(&split.pieces[m.piece])).collect();
    // The ground before anything moves (holes under props grown over from their edges).
    let ground = dilate(&tops(&terrain_only(c)));
    let mut out = c.clone();
    for (g, grp) in out.groups.iter_mut().enumerate() {
        let mut i = 0;
        grp.prims.retain(|_| {
            let keep = !gone.contains(&(g, i));
            i += 1;
            keep
        });
    }
    let mut index: HashMap<[i16; 3], u16> = HashMap::new();
    let mut fills: Vec<Prim> = Vec::new();
    let mut placed: Vec<Prim> = Vec::new();
    let donors = donors(c);
    let tint = shade(c);
    let lattice = height_lattice(c);
    // Each piece's ground where it stood (the terrain around its cells).
    let src_grounds: Vec<i32> = moves
        .iter()
        .map(|m| {
            let p = &split.pieces[m.piece];
            let under = cells_under(c, &p.prims);
            ground_of(&ground, ring(&under)).or_else(|| ground_of(&ground, under.iter().copied())).unwrap_or(p.max[1] as i32)
        })
        .collect();
    // Fill, first the ground the pieces brought (their yards and patches): the same shapes,
    // cut along the 128-unit tiles, skinned with the nearest terrain tile's texture, each corner
    // at the ground's height there (a yard on a slope stays a slope).
    for (m, &g0) in moves.iter().zip(&src_grounds).filter(|(m, _)| !m.copy) {
        for &(g, i) in &split.pieces[m.piece].prims {
            let q = c.groups[g].prims[i];
            let v = verts(c, &q);
            let on_ground = |w: &[i16; 3]| {
                let (tc, tr) = ((w[0] as i32 + 2048).div_euclid(128), (w[2] as i32 + 2048).div_euclid(128));
                (w[1] as i32 - g0).abs() <= DECAL_RISE as i32 || (gentle(&lattice, tc, tr) && (w[1] as i32 - height_at(&lattice, w[0] as i32, w[2] as i32)).abs() <= 2 * DECAL_RISE as i32)
            };
            if !v.iter().all(on_ground) {
                continue;
            }
            // Corners in polygon order (strips are 0-1-3-2).
            let ring_xz: Vec<[f32; 2]> = if q.n == 4 { [0, 1, 3, 2].iter().map(|&k| [v[k][0] as f32, v[k][2] as f32]).collect() } else { v.iter().map(|w| [w[0] as f32, w[2] as f32]).collect() };
            let (x0, x1) = (v.iter().map(|w| w[0] as i32).min().unwrap(), v.iter().map(|w| w[0] as i32).max().unwrap());
            let (z0, z1) = (v.iter().map(|w| w[2] as i32).min().unwrap(), v.iter().map(|w| w[2] as i32).max().unwrap());
            for tr in (z0 + 2048).div_euclid(128)..=((z1 + 2047).div_euclid(128)).max((z0 + 2048).div_euclid(128)) {
                for tc in (x0 + 2048).div_euclid(128)..=((x1 + 2047).div_euclid(128)).max((x0 + 2048).div_euclid(128)) {
                    let (tx, tz) = ((tc * 128 - 2048) as f32, (tr * 128 - 2048) as f32);
                    let part = clip(&ring_xz, tx, tz, 128.0);
                    if part.len() < 3 || area(&part).abs() < 1.0 {
                        continue;
                    }
                    let Some(d) = donor_for(&donors, tc, tr) else { continue };
                    for poly in fan(&part, d.facing() > 0.0) {
                        let mut f = d.prim;
                        f.n = poly.len() as u8;
                        f.list = if f.n == 4 { 7 } else { 5 };
                        let order: &[usize] = if f.n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
                        for (k, &j) in order.iter().enumerate() {
                            let [x, z] = poly[j];
                            let (x, z) = (x.round() as i32, z.round() as i32);
                            let (uv, _) = d.sample((x as f32 - tx) / 128.0, (z as f32 - tz) / 128.0);
                            let y = if gentle(&lattice, tc, tr) { height_at(&lattice, x, z) } else { g0 };
                            f.idx[k] = push_vertex(&mut out.pool, &mut index, [x, y, z])?;
                            f.uv[k] = uv;
                            f.rgb[k] = shade_at(&tint, x, z);
                        }
                        if f.n == 3 {
                            f.idx[3] = f.idx[0];
                        }
                        fills.push(f);
                    }
                }
            }
        }
    }
    // Then every cell the pieces left with no floor of the chunk's own (under walls: a house's
    // inside). Both fills sample one tile at the same places, so where they overlap they draw the
    // same texels; shape pieces wholly inside filled cells go.
    let floors = tops(&out);
    let holes: BTreeSet<usize> = left.iter().copied().filter(|&k| floors[k].is_none()).collect();
    fills.retain(|f| {
        let mut cells = BTreeSet::new();
        cells_of(&out.pool, f, &mut cells);
        !cells.iter().all(|k| holes.contains(k))
    });
    let mut tiles: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for &k in &holes {
        tiles.entry(((k % 128) as i32 / 4, (k / 128) as i32 / 4)).or_default().push(k);
    }
    for ((tc, tr), cells) in tiles {
        let Some(d) = donor_for(&donors, tc, tr) else { continue };
        let flat = (!gentle(&lattice, tc, tr)).then(|| ground_of(&ground, cells.iter().copied()).unwrap_or(c.pool[d.prim.idx[0] as usize][1] as i32));
        // One quad per rectangle of hole cells (a whole tile is one), with its part of the texture, its corners on the ground.
        for (i, j, w, h) in rectangles(cells.iter().map(|&k| ((k % 128) as i32 - tc * 4, (k / 128) as i32 - tr * 4))) {
            let mut q = d.prim;
            // Gouraud, so the corners take the ground's tint (GT4).
            q.list = 7;
            for k in 0..4 {
                let (a, b) = d.at[k];
                let (fx, fz) = ((i + a as i32 * w) as f32 / 4.0, (j + b as i32 * h) as f32 / 4.0);
                let (uv, _) = d.sample(fx, fz);
                let x = tc * 128 - 2048 + (fx * 128.0) as i32;
                let z = tr * 128 - 2048 + (fz * 128.0) as i32;
                q.idx[k] = push_vertex(&mut out.pool, &mut index, [x, flat.unwrap_or_else(|| height_at(&lattice, x, z)), z])?;
                q.uv[k] = uv;
                q.rgb[k] = shade_at(&tint, x, z);
            }
            fills.push(q);
        }
        rep.filled_cells += cells.len();
    }
    // Moved pieces; the cells their own ground covers at the new place.
    let mut own_floor: BTreeSet<usize> = BTreeSet::new();
    // Walk cells pieces write: (cell, value, whether a floor of the piece covers it rather than only a wall's foot).
    let mut writes: Vec<(usize, i16, bool)> = Vec::new();
    // Which move placed each polygon of `placed`.
    let mut placed_by: Vec<u32> = Vec::new();
    for (n, m) in moves.iter().enumerate() {
        let p = &split.pieces[m.piece];
        if m.delete {
            rep.deleted_polygons += p.prims.len();
            rep.lifts.push(0);
            continue;
        }
        let under = cells_under(c, &p.prims);
        let src_ground = src_grounds[n];
        let t0 = transform(p, m, 0);
        let dest: BTreeSet<usize> = under.iter().filter_map(|&k| t0.cell(k)).collect();
        let dst_ground = ground_of(&ground, ring(&dest)).unwrap_or(src_ground);
        let t = Transform { dy: if m.hold { 0 } else { dst_ground - src_ground }, ..t0 };
        rep.lifts.push(t.dy as i16);
        let mut floors = BTreeSet::new();
        for &(g, i) in &p.prims {
            let q = c.groups[g].prims[i];
            let v = verts(c, &q);
            // Lying on the ground where it lands (a held piece over water doesn't).
            let base = v.iter().all(|w| (w[1] as i32 + t.dy - dst_ground).abs() <= DECAL_RISE as i32);
            let mut nq = q;
            for k in 0..q.n as usize {
                let (x, z) = t.point(v[k][0] as i32, v[k][2] as i32);
                nq.idx[k] = push_vertex(&mut out.pool, &mut index, [x, v[k][1] as i32 + t.dy, z])?;
            }
            // Turned, a quad's corners keep their order, so it still faces up.
            if base {
                cells_of(&out.pool, &nq, &mut own_floor);
            }
            if flat_xz_area(&v) >= 1.0 {
                cells_of(&c.pool, &q, &mut floors);
            }
            placed.push(nq);
            placed_by.push(n as u32 + 1);
            if m.copy {
                rep.copied_polygons += 1;
            } else {
                rep.moved_polygons += 1;
            }
        }
        // Its walk cells, relative to the ground: a pit (0) or blocked (−1) stays one.
        let own = footprint(p);
        for &k in under.iter().filter(|k| own.contains(k)) {
            let Some(to) = t.cell(k) else { continue };
            let h = c.heights[k];
            let v = if h == 0 || h == -1 { h } else { (h as i32 + t.dy).clamp(i16::MIN as i32, i16::MAX as i32) as i16 };
            writes.push((to, v, floors.contains(&k)));
        }
    }
    let copies: Vec<bool> = moves.iter().map(|m| m.copy).collect();
    rep.welded_polygons = weld(&mut out, &mut placed, &placed_by, &copies);
    // Cut: terrain and fills wholly under a moved piece's own ground.
    let covered = |pool: &[[i16; 3]], q: &Prim| {
        let mut cells = BTreeSet::new();
        cells_of(pool, q, &mut cells);
        !cells.is_empty() && cells.iter().all(|k| own_floor.contains(k))
    };
    if !own_floor.is_empty() {
        let pool = out.pool.clone();
        // Read against `out`: a welded polygon may use a corner placed since.
        let terrain: Vec<Vec<bool>> = out.groups.iter().map(|g| g.prims.iter().map(|q| props::is_terrain(&out, q)).collect()).collect();
        for (grp, terrain) in out.groups.iter_mut().zip(terrain) {
            let n = grp.prims.len();
            let mut t = terrain.into_iter();
            grp.prims.retain(|q| !(t.next().unwrap_or(false) && covered(&pool, q)));
            rep.cut_polygons += n - grp.prims.len();
        }
        let n = fills.len();
        fills.retain(|q| !covered(&pool, q));
        rep.cut_polygons += n - fills.len();
    }
    rep.fill_polygons = fills.len();
    let mut buckets = Buckets::default();
    for q in fills.into_iter().chain(placed) {
        buckets.add(&out.pool, q);
    }
    let before = out.groups.len();
    out.groups.extend(buckets.0.into_values().map(|prims| Group { centre: [0; 3], prims }));
    for g in before..out.groups.len() {
        out.recompute_centre(g);
    }
    out.groups.retain(|g| !g.prims.is_empty());
    compact(&mut out);
    // Walk heights: the cells pieces left get the ground's (their footprints, and cells next to them a wall stood in
    // where the baker now reads them otherwise), then pieces write theirs.
    let (was, baked) = (bake::bake(c), bake::bake(&out));
    let cleared: Vec<usize> = left.iter().copied().filter(|k| vacated.contains(k) || baked[*k] != was[*k]).collect();
    for &k in &cleared {
        out.heights[k] = baked[k];
    }
    rep.cleared_cells = cleared.len();
    let written: BTreeSet<usize> = writes.iter().map(|w| w.0).collect();
    // A cell a piece's floor covers wins over one only a wall's foot stands in (a railing's side on the deck's row).
    writes.sort_by_key(|w| w.2);
    for (k, v, _) in writes {
        out.heights[k] = v;
    }
    rep.written_cells = written.len();
    debug_assert!(out.heights.len() == CELLS);
    out.sizes()?;
    Ok((out, rep))
}

/* ───────────────────────── Persisting moves (research 12 §6.3) ───────────────────────── */

/// A piece's identity across split revisions: a hash of its polygons' vertex
/// positions (each polygon's sorted, then the polygons sorted), with its
/// polygon count and bounds for a near match. Indices aren't stable: a
/// better split rule renumbers the pieces, and the same house keeps its print.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    /// FNV-1a 64, 16 hex digits.
    pub hash: String,
    pub polygons: u32,
    pub min: [i16; 3],
    pub max: [i16; 3],
}

/// The fingerprint of piece `p` of a split of `c`.
pub fn fingerprint(c: &Chunk, p: &Piece) -> Fingerprint {
    let mut shapes: Vec<Vec<[i16; 3]>> = p
        .prims
        .iter()
        .map(|&(g, i)| {
            let mut v = verts(c, &c.groups[g].prims[i]);
            v.sort();
            v
        })
        .collect();
    shapes.sort();
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    let mut eat = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01B3);
    };
    for s in &shapes {
        // The corner count keeps a triangle and a quad over the same points apart.
        eat(s.len() as u8);
        for v in s {
            for a in v {
                a.to_le_bytes().into_iter().for_each(&mut eat);
            }
        }
    }
    Fingerprint { hash: format!("{h:016x}"), polygons: p.prims.len() as u32, min: p.min, max: p.max }
}

/// Every piece's fingerprint, in split order.
pub fn fingerprints(c: &Chunk, split: &Split) -> Vec<Fingerprint> {
    split.pieces.iter().map(|p| fingerprint(c, p)).collect()
}

/// How much `a` and `b` are one piece (0–1): the overlap of their XZ bounds
/// over their union, when their polygon counts are within a quarter of each other.
fn likeness(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let (pa, pb) = (a.polygons.max(1) as f32, b.polygons.max(1) as f32);
    if pa.min(pb) / pa.max(pb) < NEAR_POLYGONS {
        return 0.0;
    }
    let span = |lo: i16, hi: i16| (hi as f32 - lo as f32).max(1.0);
    let ov = |a0: i16, a1: i16, b0: i16, b1: i16| (a1.min(b1) as f32 - a0.max(b0) as f32).max(0.0);
    let inter = ov(a.min[0], a.max[0], b.min[0], b.max[0]) * ov(a.min[2], a.max[2], b.min[2], b.max[2]);
    let union = span(a.min[0], a.max[0]) * span(a.min[2], a.max[2]) + span(b.min[0], b.max[0]) * span(b.min[2], b.max[2]) - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Near matches: polygon counts within this ratio, bounds overlapping by at least [`NEAR_OVERLAP`].
const NEAR_POLYGONS: f32 = 0.75;
const NEAR_OVERLAP: f32 = 0.75;

/// Which of `pieces` each of `stored` names, one piece per print: the same
/// hash first; then, for a print a split revision changed (a decal or a post
/// went to a neighbour), the piece left over most like it (bounds overlapping
/// by ¾ of their union, polygon counts within a quarter). `None`: no piece of
/// this split is it any more.
pub fn match_prints(stored: &[Fingerprint], pieces: &[Fingerprint]) -> Vec<Option<usize>> {
    let mut out: Vec<Option<usize>> = vec![None; stored.len()];
    let mut taken = vec![false; pieces.len()];
    let mut by_hash: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in pieces.iter().enumerate() {
        by_hash.entry(p.hash.as_str()).or_default().push(i);
    }
    for (k, s) in stored.iter().enumerate() {
        if let Some(i) = by_hash.get(s.hash.as_str()).and_then(|v| v.iter().copied().find(|&i| !taken[i])) {
            taken[i] = true;
            out[k] = Some(i);
        }
    }
    let mut near: Vec<(f32, usize, usize)> = Vec::new();
    for (k, s) in stored.iter().enumerate().filter(|(k, _)| out[*k].is_none()) {
        for (i, p) in pieces.iter().enumerate().filter(|(i, _)| !taken[*i]) {
            let l = likeness(s, p);
            if l >= NEAR_OVERLAP {
                near.push((l, k, i));
            }
        }
    }
    near.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, k, i) in near {
        if out[k].is_none() && !taken[i] {
            taken[i] = true;
            out[k] = Some(i);
        }
    }
    out
}

/// A move as a project keeps it: the piece by its fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PieceMove {
    pub piece: Fingerprint,
    #[serde(default)]
    pub dc: i16,
    #[serde(default)]
    pub dr: i16,
    #[serde(default)]
    pub turns: u8,
    #[serde(default)]
    pub delete: bool,
    /// A copy of the piece goes there; the piece stays (Duplicate, Repeat).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub copy: bool,
    /// It keeps its height rather than standing on the ground there (a span over water).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hold: bool,
}

impl PieceMove {
    /// It changes nothing: the piece stays where it is, or it is a copy deleted or put on the piece itself.
    pub fn is_noop(&self) -> bool {
        (self.copy && self.delete) || (!self.delete && self.dc == 0 && self.dr == 0 && self.turns % 4 == 0)
    }
}

/// `moves` against a split's pieces (`prints`, [`fingerprints`]): the index
/// moves [`rebake`] takes, in the stored order, and for each stored move the
/// piece it names (`None`: no piece matches, so it does nothing). Moves that
/// leave a piece where it is name it but aren't returned, so the chunk stays
/// as it was. A move or delete names a piece no other one does; copies name
/// theirs whatever else does (a row of copies, of a piece that moves too).
pub fn resolve(prints: &[Fingerprint], moves: &[PieceMove]) -> (Vec<Move>, Vec<Option<usize>>) {
    let own: Vec<usize> = (0..moves.len()).filter(|&k| !moves[k].copy).collect();
    let stored: Vec<Fingerprint> = own.iter().map(|&k| moves[k].piece.clone()).collect();
    let mut named = vec![None; moves.len()];
    for (&k, n) in own.iter().zip(match_prints(&stored, prints)) {
        named[k] = n;
    }
    for (k, m) in moves.iter().enumerate().filter(|(_, m)| m.copy) {
        named[k] = match_prints(std::slice::from_ref(&m.piece), prints)[0];
    }
    let out = moves
        .iter()
        .zip(&named)
        .filter_map(|(m, n)| n.filter(|_| !m.is_noop()).map(|piece| Move { piece, dc: m.dc, dr: m.dr, turns: m.turns % 4, delete: m.delete, copy: m.copy, hold: m.hold }))
        .collect();
    (out, named)
}

/* ───────────────────────── Carrying pack records (research 12 §3.3) ───────────────────────── */

/// Record types a move carries: door actors (2), examine spots (4) and smoke (5).
pub const CARRIED_KINDS: [u8; 3] = [2, 4, 5];
/// How far outside a piece's bounds a record still belongs to it.
pub const CARRY_MARGIN: i32 = 64;

/// One placing branch of a record of an NPC pack homed in a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spot {
    pub slot: u32,
    /// The record's type (`d >> 8`): 2 door actor, 4 examine spot, 5 smoke, else a character.
    pub kind: u8,
    /// D-relative offset of the branch's condition word.
    pub at: u32,
    pub place: Place,
}

/// Every placing branch of the records of `pack` (an NPC pack without brought
/// characters: [`super::character::base`]) homed in `chunk`, the party left
/// out, once per branch (records sharing a program name it once).
pub fn spots(pack: &[u8], chunk: u32) -> Vec<Spot> {
    let Some(np) = NpcPack::parse(pack) else { return Vec::new() };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (slot, a) in np.actors.iter().enumerate().skip(2) {
        if a.b & 0xFFFF != chunk {
            continue;
        }
        let kind = ((a.d >> 8) & 0xFF) as u8;
        for br in np.placement(pack, slot) {
            let Outcome::Place { x, h, z, dir } = br.outcome else { continue };
            if seen.insert(br.at) {
                out.push(Spot { slot: slot as u32, kind, at: br.at, place: Place { x, h, z, dir } });
            }
        }
    }
    out
}

/// The piece a point placed by position belongs to: the nearest whose bounds
/// it lies within [`CARRY_MARGIN`] of (inside counts as 0), the smaller on a tie.
pub fn owner_of(split: &Split, x: i32, z: i32) -> Option<usize> {
    let gap = |lo: i16, hi: i16, v: i32| (lo as i32 - v).max(v - hi as i32).max(0);
    split
        .pieces
        .iter()
        .enumerate()
        .filter_map(|(n, p)| {
            let d = gap(p.min[0], p.max[0], x).max(gap(p.min[2], p.max[2], z));
            let area = (p.max[0] as i64 - p.min[0] as i64) * (p.max[2] as i64 - p.min[2] as i64);
            (d <= CARRY_MARGIN).then_some((d, area, n))
        })
        .min()
        .map(|(_, _, n)| n)
}

/// A placing branch a move carried, as the project keeps it: undoing the move
/// writes `from` back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Carried {
    /// `npc/n`.
    pub pack: u32,
    /// D-relative offset of the branch's condition word.
    pub at: u32,
    /// Its words before the move carried it, and what the last rebake wrote.
    pub from: Place,
    pub to: Place,
}

/// Where branch `s` goes with `moves` (`lifts` from [`rebake`], one per move),
/// or `None` when its piece stays or goes (a deleted piece's records are
/// taken out instead, [`take_pack`], their words as they were before).
fn carried_to(split: &Split, moves: &[Move], lifts: &[i16], s: &Spot, from: Place) -> Option<Place> {
    if !CARRIED_KINDS.contains(&s.kind) {
        return None;
    }
    let owner = owner_of(split, from.x as i32, from.z as i32)?;
    let (n, m) = moves.iter().enumerate().find(|(_, m)| m.piece == owner && !m.delete && !m.copy)?;
    let lift = lifts.get(n).copied().unwrap_or(0);
    let t = transform(&split.pieces[owner], m, lift as i32);
    let (x, z) = t.point(from.x as i32, from.z as i32);
    Some(Place { x: x as i16, h: from.h.saturating_add(lift), z: z as i16, dir: t.dir(from.dir) })
}

/// The writes that bring pack `pack_id` (`pack`, as the project has it,
/// without brought characters) to `moves`, and the carried list to keep for it.
///
/// - A door actor, examine spot or smoke whose piece moves goes with it, from
///   where it stood before any move (`before` remembers that).
/// - One a move no longer carries goes back to `from`, unless something else
///   moved it since (its words aren't the `to` the last rebake wrote): then it
///   stays where it was put, and the pieces let go of it.
pub fn carry_pack(pack_id: u32, pack: &[u8], chunk: u32, split: &Split, moves: &[Move], lifts: &[i16], before: &[Carried]) -> (Vec<PlaceWrite>, Vec<Carried>) {
    let prev: HashMap<u32, &Carried> = before.iter().filter(|c| c.pack == pack_id).map(|c| (c.at, c)).collect();
    let mut writes = Vec::new();
    let mut kept = Vec::new();
    for s in spots(pack, chunk) {
        let mine = prev.get(&s.at).filter(|c| c.to == s.place);
        let from = mine.map_or(s.place, |c| c.from);
        match carried_to(split, moves, lifts, &s, from) {
            Some(to) => {
                if to != s.place {
                    writes.push(PlaceWrite { at: s.at, place: Some(to) });
                }
                kept.push(Carried { pack: pack_id, at: s.at, from, to });
            }
            None if mine.is_some() && from != s.place => writes.push(PlaceWrite { at: s.at, place: Some(from) }),
            None => {}
        }
    }
    (writes, kept)
}

/* ───────────────────────── Taking a deleted piece's records out ───────────────────────── */

/// A record a delete took out of the game ([`super::remove`]): the door,
/// examine spot or smoke of a deleted piece, as the project keeps it. Putting
/// the piece back puts the record back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Taken {
    /// `npc/n`.
    pub pack: u32,
    pub slot: u32,
    /// Put back in the game by hand while its piece stays deleted: the delete
    /// lets go of it and doesn't take it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub let_go: bool,
}

/// Why deleting a piece leaves one of its doors, examine spots or smoke
/// standing (screen 10's inspector words them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stays {
    /// An actor the project added (a copy): copies stay.
    Added,
    /// Its placement ladder (a door) or its program (an examine spot, smoke)
    /// is edited in the project: taking it out would lose that.
    Edited,
    /// Out of the game already (removed by hand): the delete leaves it so.
    Removed,
    /// One of its branches stands by another piece, or by none.
    Spread,
    /// Put back in the game by hand while the piece was deleted.
    Kept,
}

/// A door, examine spot or smoke record of a pack homed in a chunk, as a
/// delete sees it.
#[derive(Debug, Clone)]
pub struct Record {
    pub slot: u32,
    /// Its retail type: 2, 4 or 5 ([`CARRIED_KINDS`]).
    pub kind: u8,
    /// Its placing branches (D offset, words) where they stood before any move;
    /// a door taken out reads them from its retail ladder.
    pub branches: Vec<(u32, Place)>,
    /// Out of the game now ([`super::remove`]), and hidden (smoke).
    removed: bool,
    hidden: bool,
    /// A retail record whose own words a removal leaves as they are: put back,
    /// it is the bytes it was.
    plain: bool,
    /// The project added it ([`super::actor`]).
    added: bool,
}

impl Record {
    /// Out of the game the way a delete takes it out.
    fn taken_out(&self) -> bool {
        self.removed && (self.kind != super::remove::SMOKE || self.hidden)
    }

    /// Every branch belongs to a piece `gone` says goes.
    fn goes(&self, split: &Split, gone: impl Fn(usize) -> bool) -> bool {
        !self.branches.is_empty() && self.branches.iter().all(|(_, p)| owner_of(split, p.x as i32, p.z as i32).is_some_and(&gone))
    }

    /// Why deleting piece `owner` alone leaves it (`taken`: the project's list for its pack), or `None`: it goes.
    pub fn stays(&self, split: &Split, owner: usize, taken: &[Taken]) -> Option<Stays> {
        let mine = taken.iter().find(|t| t.slot == self.slot);
        if self.added {
            Some(Stays::Added)
        } else if mine.is_some_and(|t| t.let_go) {
            Some(Stays::Kept)
        } else if mine.is_none() && self.removed {
            Some(Stays::Removed)
        } else if mine.is_none() && !self.plain {
            Some(Stays::Edited)
        } else if !self.goes(split, |o| o == owner) {
            Some(Stays::Spread)
        } else {
            None
        }
    }
}

/// The door, examine spot and smoke records of `pack` (as the project has it,
/// without brought characters) homed in `chunk`, against `retail`.
/// `carried`: the project's carried list (a carried branch counts where it
/// stood before the move); `taken`: its taken list for this pack (doors it
/// took out are read from their retail ladders; ones taken out otherwise
/// aren't listed).
pub fn records(pack_id: u32, retail: &[u8], pack: &[u8], chunk: u32, carried: &[Carried], taken: &[Taken]) -> Vec<Record> {
    let (Some(r), Some(c)) = (NpcPack::parse(retail), NpcPack::parse(pack)) else { return Vec::new() };
    let cd = c.d_block(pack);
    let before: HashMap<u32, &Carried> = carried.iter().filter(|k| k.pack == pack_id).map(|k| (k.at, k)).collect();
    let ours: HashSet<u32> = taken.iter().filter(|t| t.pack == pack_id && !t.let_go).map(|t| t.slot).collect();
    let mut out = Vec::new();
    for (slot, a) in c.actors.iter().enumerate().skip(2) {
        let ra = r.actors.get(slot);
        let kind = ((ra.map_or(a.d, |ra| ra.d) >> 8) & 0xFF) as u8;
        if a.b & 0xFFFF != chunk || !CARRIED_KINDS.contains(&kind) {
            continue;
        }
        // Doors are taken out by their placement, objects by their program ([`super::remove::how`]).
        let object = kind != 2;
        let (removed, hidden, plain) = match ra {
            Some(ra) if object => (super::remove::is_end(cd, a.script, r.d_span), kind == super::remove::SMOKE && (a.d >> 8) as u8 == 4, a.script == ra.script),
            Some(ra) => (super::remove::placement_removed(cd, a.placement, r.d_span), false, a.placement == ra.placement),
            None => (false, false, false),
        };
        // A door out of the game points at the removal block; its own ladder is still there.
        let ladder = match ra {
            Some(ra) if removed && !object => {
                if !ours.contains(&(slot as u32)) {
                    continue;
                }
                ra.placement
            }
            _ => a.placement,
        };
        let branches = crate::trigger::placement_ladder(cd, ladder)
            .into_iter()
            .filter_map(|br| match br.outcome {
                Outcome::Place { x, h, z, dir } => {
                    let now = Place { x, h, z, dir };
                    Some((br.at, before.get(&br.at).filter(|k| k.to == now).map_or(now, |k| k.from)))
                }
                _ => None,
            })
            .collect();
        out.push(Record { slot: slot as u32, kind, branches, removed, hidden, plain, added: ra.is_none() });
    }
    out
}

/// Which records of pack `pack_id` the deletes in `moves` take out
/// ([`records`]' arguments): the slots to take out and to put back, and the
/// taken list to keep for the pack.
///
/// - A door, examine spot or smoke goes out when every branch belongs to a
///   deleted piece, unless it is an added actor, edited ([`Stays::Edited`]) or
///   out of the game already.
/// - One taken out goes back when no delete takes it any more.
/// - One taken out but back in the game (put back by hand) is let go of, and
///   not taken again while its piece stays deleted.
#[allow(clippy::too_many_arguments)]
pub fn take_pack(pack_id: u32, retail: &[u8], pack: &[u8], chunk: u32, split: &Split, moves: &[Move], carried: &[Carried], taken: &[Taken]) -> (BTreeSet<u32>, BTreeSet<u32>, Vec<Taken>) {
    let deleted: HashSet<usize> = moves.iter().filter(|m| m.delete).map(|m| m.piece).collect();
    let prev: HashMap<u32, &Taken> = taken.iter().filter(|t| t.pack == pack_id).map(|t| (t.slot, t)).collect();
    let (mut take, mut give, mut kept) = (BTreeSet::new(), BTreeSet::new(), Vec::new());
    for rec in records(pack_id, retail, pack, chunk, carried, taken) {
        let goes = rec.goes(split, |o| deleted.contains(&o));
        let keep = |let_go| Taken { pack: pack_id, slot: rec.slot, let_go };
        match prev.get(&rec.slot) {
            Some(t) if t.let_go => {
                if goes {
                    kept.push(keep(true));
                }
            }
            // Ours, and still out: it stays out while its piece is deleted.
            Some(_) if rec.taken_out() => {
                if goes {
                    kept.push(keep(false));
                } else {
                    give.insert(rec.slot);
                }
            }
            // Ours, but put back by hand since.
            Some(_) => {
                if goes {
                    kept.push(keep(true));
                }
            }
            None if goes && !rec.added && !rec.removed && rec.plain => {
                take.insert(rec.slot);
                kept.push(keep(false));
            }
            None => {}
        }
    }
    (take, give, kept)
}

/// What a rebake does to pack `pack_id` ([`bake_pack`]).
#[derive(Debug, Clone)]
pub struct PackBaked {
    /// The pack to save (`None`: as it was).
    pub bytes: Option<Vec<u8>>,
    pub carried: Vec<Carried>,
    pub taken: Vec<Taken>,
}

/// Pack `pack_id` (`retail`, and `current` as the project has it) brought to
/// `moves` (`lifts` from [`rebake`]) for the records homed in `chunk`, with
/// the lists the project keeps (`carried`, `taken`: their last values):
/// records no delete takes any more come back first, so their branches can be
/// carried; then branches go with moved pieces or back to their words from
/// before ([`carry_pack`]); then a deleted piece's records go out
/// ([`take_pack`], [`super::remove::take_out`]), their branches where they
/// stood.
#[allow(clippy::too_many_arguments)]
pub fn bake_pack(pack_id: u32, retail: &[u8], current: &[u8], chunk: u32, split: &Split, moves: &[Move], lifts: &[i16], carried: &[Carried], taken: &[Taken]) -> Result<PackBaked> {
    let plain = super::character::base(retail, current)?;
    let (take, give, taken) = take_pack(pack_id, retail, &plain, chunk, split, moves, carried, taken);
    let mut now = if give.is_empty() { current.to_vec() } else { super::remove::take_out(retail, current, &BTreeSet::new(), &give)? };
    let plain = super::character::base(retail, &now)?.into_owned();
    let (writes, carried) = carry_pack(pack_id, &plain, chunk, split, moves, lifts, carried);
    if !writes.is_empty() {
        now = super::placement::apply(retail, &now, &writes)?;
    }
    if !take.is_empty() {
        now = super::remove::take_out(retail, &now, &take, &BTreeSet::new())?;
    }
    Ok(PackBaked { bytes: (now != current).then_some(now), carried, taken })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::chunk::Group;

    fn quad(idx: [u16; 4], own: bool) -> Prim {
        Prim {
            list: 7,
            code: 0x3C,
            n: 4,
            idx,
            rgb: [[128; 3]; 4],
            uv: [[0, 0], [31, 0], [0, 31], [31, 31]],
            clut: 0x7802,
            tpage: if own { 0x0A } else { 0x08 },
        }
    }

    /// A 3 × 1 strip of 128-unit grass tiles at y = −128 with a 64-unit box on the middle one, cut out of it.
    fn sample() -> Chunk {
        let mut c = Chunk::blank(0);
        let y = -128;
        let mut pool = Vec::new();
        for x in [0i16, 128, 256, 384] {
            pool.push([x, y, 0]);
            pool.push([x, y, 128]);
        }
        // Box: a wall standing on the middle tile's south edge (z 0), and its top.
        pool.extend([[160, y, 0], [224, y, 0], [160, y - 64, 0], [224, y - 64, 0], [160, y - 64, 64], [224, y - 64, 64]]);
        c.pool = pool;
        let tiles = vec![quad([0, 2, 1, 3], false), quad([4, 6, 5, 7], false)];
        let props = vec![quad([10, 11, 8, 9], true), quad([10, 11, 12, 13], true)];
        c.groups.push(Group { centre: [0; 3], prims: tiles });
        c.groups.push(Group { centre: [0; 3], prims: props });
        c.heights = bake::bake(&c);
        c
    }

    #[test]
    fn a_prop_is_one_piece_and_terrain_isnt_moved() {
        let c = sample();
        let s = split(&c);
        assert_eq!(s.pieces.len(), 1);
        assert_eq!(s.pieces[0].prims, vec![(1, 0), (1, 1)]);
        assert!(!s.pieces[0].fixed);
    }

    #[test]
    fn a_part_floating_on_a_roof_rides_on_it() {
        // A chimney cap on a corner of the box's top (off its centre, so not a crown on a trunk): no shared
        // edge, and 64 units above the ground.
        let mut c = sample();
        // A roof 96 units up (the sample box is 64 tall, no higher than a chimney may float).
        let y = -224;
        for v in c.pool.iter_mut().filter(|v| v[1] == -192) {
            v[1] = y;
        }
        let at = c.pool.len() as u16;
        c.pool.extend([[160, y, 0], [184, y, 0], [160, y, 24], [184, y, 24]]);
        c.groups[1].prims.push(quad([at, at + 1, at + 2, at + 3], true));
        let s = split(&c);
        assert_eq!(s.pieces.len(), 1, "the cap goes with the box");
        assert_eq!(s.pieces[0].prims.len(), 3);
        // Lifted clear of the roof (more than its own height above it), it is something else: it stays apart.
        for v in c.pool.iter_mut().skip(at as usize) {
            v[1] = y - 40;
        }
        assert_eq!(split(&c).pieces.len(), 2);
    }

    #[test]
    fn moving_a_prop_fills_its_hole_and_takes_its_cells() {
        let c = sample();
        let s = split(&c);
        let (out, rep) = rebake(&c, &s, &[Move { piece: 0, dr: 2, ..Move::default() }]).unwrap();
        assert_eq!(rep.moved_polygons, 2);
        // The box's top covered cells over the middle tile, which had no terrain: they are ground again.
        assert!(rep.filled_cells > 0);
        let h = bake::bake(&out);
        let at = |x: i32, z: i32| cell_of(x, z).unwrap();
        assert_eq!(h[at(176, 16)], -129, "where the box was is walkable ground");
        // The box top moved 64 units north.
        assert_eq!(out.heights[at(176, 80)], c.heights[at(176, 16)]);
        let ys: Vec<i16> = out.pool.iter().filter(|v| v[2] == 128 && v[0] == 160).map(|v| v[1]).collect();
        assert!(ys.contains(&-192), "top's south edge is at z 128 now: {ys:?}");
    }

    fn print(hash: &str, polygons: u32, min: [i16; 3], max: [i16; 3]) -> Fingerprint {
        Fingerprint { hash: hash.into(), polygons, min, max }
    }

    #[test]
    fn a_fingerprint_ignores_polygon_order_and_groups() {
        let c = sample();
        let s = split(&c);
        let a = fingerprint(&c, &s.pieces[0]);
        // The same box, its two polygons swapped and one put in a group of its own, corners renumbered.
        let mut d = c.clone();
        let props = d.groups.pop().unwrap();
        d.groups.push(Group { centre: [0; 3], prims: vec![props.prims[1]] });
        d.groups.insert(0, Group { centre: [0; 3], prims: vec![props.prims[0]] });
        d.pool.reverse();
        let n = d.pool.len() as u16;
        for g in d.groups.iter_mut() {
            for q in g.prims.iter_mut() {
                for i in q.idx.iter_mut() {
                    *i = n - 1 - *i;
                }
            }
        }
        let sd = split(&d);
        assert_eq!(sd.pieces.len(), 1);
        assert_ne!(sd.pieces[0].prims, s.pieces[0].prims, "the indices changed");
        assert_eq!(fingerprint(&d, &sd.pieces[0]), a);
        // One corner one unit over is another piece.
        let mut e = c.clone();
        let k = e.pool.iter().position(|v| *v == [224, -192, 64]).unwrap();
        e.pool[k][0] += 1;
        assert_ne!(fingerprint(&e, &split(&e).pieces[0]).hash, a.hash);
    }

    #[test]
    fn prints_match_by_hash_first_then_by_likeness() {
        let house = print("aa", 200, [0, -300, 0], [512, 0, 384]);
        let barrel = print("bb", 12, [600, -40, 600], [632, 0, 632]);
        let well = print("cc", 40, [-500, -80, -500], [-400, 0, -400]);
        // A later split: the house gained a decal (one polygon, a little wider), the barrel is gone.
        let house2 = print("dd", 201, [0, -300, 0], [528, 0, 384]);
        let pieces = vec![well.clone(), house2, print("ee", 12, [900, -40, 900], [932, 0, 932])];
        assert_eq!(match_prints(&[house.clone(), barrel.clone(), well.clone()], &pieces), vec![Some(1), None, Some(0)]);
        // Each piece is named once: a second print of the well gets nothing.
        assert_eq!(match_prints(&[well.clone(), well.clone()], &pieces), vec![Some(0), None]);
        // An exact hash wins over a likelier near match.
        let twin = print("aa", 200, [2000, -300, 2000], [2040, 0, 2040]);
        assert_eq!(match_prints(&[house.clone()], &[house2_like(&house), twin]), vec![Some(1)]);
        // Too few polygons in common, or bounds too far apart, is no match.
        assert_eq!(match_prints(&[house.clone()], &[print("ff", 120, [0, -300, 0], [512, 0, 384])]), vec![None]);
        assert_eq!(match_prints(&[house], &[print("ff", 200, [256, -300, 0], [768, 0, 384])]), vec![None]);
    }

    fn house2_like(h: &Fingerprint) -> Fingerprint {
        Fingerprint { hash: "zz".into(), ..h.clone() }
    }

    #[test]
    fn resolve_names_every_move_and_drops_the_ones_that_stay() {
        let c = sample();
        let s = split(&c);
        let p = fingerprint(&c, &s.pieces[0]);
        let stay = PieceMove { piece: p.clone(), dc: 0, dr: 0, turns: 4, delete: false, copy: false, hold: false };
        let lost = PieceMove { piece: print("00", 2, [1000; 3], [1100; 3]), dc: 1, dr: 0, turns: 0, delete: false, copy: false, hold: false };
        let (moves, named) = resolve(&fingerprints(&c, &s), &[stay, lost]);
        assert!(moves.is_empty());
        assert_eq!(named, vec![Some(0), None]);
        let (moves, _) = resolve(&fingerprints(&c, &s), &[PieceMove { piece: p, dc: 0, dr: 2, turns: 5, delete: false, copy: false, hold: false }]);
        assert_eq!(moves, vec![Move { piece: 0, dr: 2, turns: 1, ..Move::default() }]);
    }

    #[test]
    fn a_move_out_of_the_chunk_is_refused() {
        let c = sample();
        let s = split(&c);
        let e = rebake(&c, &s, &[Move { piece: 0, dc: 70, ..Move::default() }]).unwrap_err();
        assert_eq!(e.code, "UNBAKE_OUTSIDE");
    }

    #[test]
    fn hole_cells_fill_as_few_rectangles() {
        let all: Vec<(i32, i32)> = (0..4).flat_map(|j| (0..4).map(move |i| (i, j))).collect();
        assert_eq!(rectangles(all.clone()), vec![(0, 0, 4, 4)]);
        // An L: the bottom row, then the left column above it.
        let l = [(0, 0), (1, 0), (2, 0), (3, 0), (0, 1), (0, 2), (0, 3)];
        assert_eq!(rectangles(l), vec![(0, 0, 4, 1), (0, 1, 1, 3)]);
        // Every cell is covered exactly once.
        let some = [(1, 1), (2, 1), (1, 2), (2, 2), (3, 3), (0, 3)];
        let mut n = 0;
        for (i, j, w, h) in rectangles(some) {
            n += w * h;
            assert!((i..i + w).all(|x| (j..j + h).all(|y| some.contains(&(x, y)))));
        }
        assert_eq!(n, some.len() as i32);
    }

    #[test]
    fn records_belong_to_the_nearest_piece() {
        let mk = |min: [i16; 3], max: [i16; 3]| Piece { prims: vec![], decals: 0, min, max, fixed: false, cut: None };
        let s = Split { pieces: vec![mk([0, -300, 0], [512, 0, 384]), mk([520, -40, 0], [552, 0, 32])], ground: vec![], kinds: BTreeMap::new() };
        // Inside the house; inside the barrel (also within the house's margin); between them, nearer the barrel.
        assert_eq!(owner_of(&s, 100, 100), Some(0));
        assert_eq!(owner_of(&s, 530, 10), Some(1));
        assert_eq!(owner_of(&s, 518, 200), Some(0));
        assert_eq!(owner_of(&s, 700, 700), None);
    }

    #[test]
    fn turns_keep_the_footprint_corner() {
        let p = Piece { prims: vec![], decals: 0, min: [0, -10, 0], max: [64, 0, 32], fixed: false, cut: None };
        let t = transform(&p, &Move { piece: 0, turns: 1, ..Move::default() }, 0);
        // (64, 0) — the far x corner — turns to (32, 64) in a 32 × 64 footprint.
        assert_eq!(t.point(64, 0), (32, 64));
        assert_eq!(t.point(0, 32), (0, 0));
        assert_eq!(t.dir(1024), 0);
    }
    #[test]
    fn a_cut_takes_what_its_box_and_band_hold() {
        let c = sample();
        // The box's top (y −192) and not its wall: the band stops above the wall's centre (y −160).
        let top = Cut { min: [128, -200, -32], max: [256, -180, 96] };
        let s = split_cut(&c, &[top]);
        assert_eq!(s.pieces.len(), 2);
        let cut = s.pieces.iter().find(|p| p.cut == Some(0)).unwrap();
        assert_eq!(cut.prims, vec![(1, 1)]);
        assert_eq!(s.pieces.iter().find(|p| p.cut.is_none()).unwrap().prims, vec![(1, 0)]);
        // A polygon on the box's wall isn't inside: the wall stands on z 0, so a box from z 0 leaves it.
        let wall = Cut { min: [128, -256, 0], max: [256, 0, 96] };
        assert!(split_cut(&c, &[wall]).pieces.iter().all(|p| p.cut.is_none() || !p.prims.contains(&(1, 0))));
        // An earlier cut keeps what it took; a cut that holds nothing makes no piece.
        let all = Cut { min: [128, -256, -32], max: [256, 0, 96] };
        let s = split_cut(&c, &[top, all, Cut { min: [1000, -10, 1000], max: [1100, 10, 1100] }]);
        assert_eq!(s.pieces.iter().map(|p| (p.cut, p.prims.len())).collect::<Vec<_>>(), vec![(Some(0), 1), (Some(1), 1)]);
        // The fingerprints stay apart, so moves keep naming the cut pieces.
        let prints = fingerprints(&c, &s);
        assert_ne!(prints[0].hash, prints[1].hash);
    }

    #[test]
    fn a_copy_leaves_the_piece_and_hold_keeps_its_height() {
        let c = sample();
        let s = split(&c);
        let (out, rep) = rebake(&c, &s, &[Move { piece: 0, dr: 2, copy: true, ..Move::default() }]).unwrap();
        assert_eq!((rep.copied_polygons, rep.moved_polygons, rep.filled_cells, rep.cleared_cells), (2, 0, 0, 0));
        let at = |x: i32, z: i32| cell_of(x, z).unwrap();
        // The box is still there, and a copy of it 64 units north.
        assert!(out.pool.contains(&[160, -192, 0]) && out.pool.contains(&[160, -192, 64]) && out.pool.contains(&[160, -192, 128]));
        assert_eq!(out.heights[at(176, 16)], c.heights[at(176, 16)]);
        assert_eq!(out.heights[at(176, 80)], c.heights[at(176, 16)]);
        // Copied onto ground 128 lower (a tile far off at y 0) it sinks with it, unless it holds its height.
        let mut low = c.clone();
        let a = low.pool.len() as u16;
        low.pool.extend([[1024, 0, 1024], [1152, 0, 1024], [1024, 0, 1152], [1152, 0, 1152]]);
        low.groups[0].prims.push(quad([a, a + 1, a + 2, a + 3], false));
        let s = split(&low);
        let far = Move { piece: 0, dc: 28, dr: 33, copy: true, ..Move::default() };
        let sank = rebake(&low, &s, &[far]).unwrap();
        assert_eq!(sank.1.lifts, vec![128]);
        assert!(sank.0.pool.contains(&[1056, -64, 1056]));
        let held = rebake(&low, &s, &[Move { hold: true, ..far }]).unwrap();
        assert_eq!(held.1.lifts, vec![0]);
        assert!(held.0.pool.contains(&[1056, -192, 1056]), "the copy's top stays at y −192");
        // A copy is never a delete.
        assert_eq!(rebake(&c, &split(&c), &[Move { piece: 0, delete: true, copy: true, ..Move::default() }]).unwrap_err().code, "UNBAKE_COPY");
    }

    /// The sample with a floating 64-unit deck slice at x 1000: a quad and a triangle, Tamsoft's split of a strip
    /// whose east edge has a corner at z 32 the west edge hasn't (as chunk 42's bridge has at z −1717).
    fn slice() -> Chunk {
        let mut c = sample();
        let (x, y) = (1024i16, -300i16);
        let at = c.pool.len() as u16;
        c.pool.extend([[x, y, 0], [x + 64, y, 0], [x + 64, y, 32], [x, y, 64], [x + 64, y, 64]]);
        let mut tri = quad([at, at + 1, at + 2, at], true);
        (tri.n, tri.list) = (3, 5);
        c.groups[1].prims.push(tri);
        c.groups[1].prims.push(quad([at, at + 2, at + 3, at + 4], true));
        c
    }

    #[test]
    fn copies_side_by_side_are_welded() {
        let c = slice();
        let s = split(&c);
        let n = s.pieces.iter().position(|p| p.min[0] == 1024).unwrap();
        assert_eq!(s.pieces[n].prims.len(), 2);
        let row: Vec<Move> = (1..=2).map(|k| Move { piece: n, dc: 2 * k, copy: true, hold: true, ..Move::default() }).collect();
        let (out, rep) = rebake(&c, &s, &row).unwrap();
        // Each copy's quad has the corner before it (x 1088 and 1152, z 32) on its west edge: it becomes 3 triangles.
        assert_eq!(rep.welded_polygons, 2);
        let deck: Vec<&Prim> = out.groups.iter().flat_map(|g| &g.prims).filter(|p| out.pool[p.idx[0] as usize][1] == -300).collect();
        assert_eq!(deck.len(), 2 + 2 * (1 + 3));
        // No corner lies inside any deck edge any more.
        for p in &deck {
            let ring: &[usize] = if p.n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
            for e in 0..ring.len() {
                let (a, b) = (out.pool[p.idx[ring[e]] as usize], out.pool[p.idx[ring[(e + 1) % ring.len()]] as usize]);
                for q in &deck {
                    for &i in &q.idx[..q.n as usize] {
                        assert!(on_edge(a, b, out.pool[i as usize]).is_none(), "T-junction at {:?} on {a:?}–{b:?}", out.pool[i as usize]);
                    }
                }
            }
        }
        // The split triangles keep the quad's texture where they meet the new corner, halfway up its west edge.
        let mid = out.pool.iter().position(|v| *v == [1088, -300, 32]).unwrap() as u16;
        let uvs: BTreeSet<[u8; 2]> = deck.iter().filter(|p| p.list == 5).flat_map(|p| (0..3).filter(|&k| p.idx[k] == mid).map(|k| p.uv[k])).collect();
        assert!(uvs.contains(&[0, 16]), "{uvs:?}");
    }

    #[test]
    fn copies_name_their_piece_whatever_else_does() {
        let c = sample();
        let s = split(&c);
        let p = fingerprint(&c, &s.pieces[0]);
        let mv = |dr: i16, copy: bool| PieceMove { piece: p.clone(), dc: 0, dr, turns: 0, delete: false, copy, hold: false };
        let (moves, named) = resolve(&fingerprints(&c, &s), &[mv(1, true), mv(2, false), mv(3, true)]);
        assert_eq!(named, vec![Some(0); 3]);
        assert_eq!(moves.iter().map(|m| (m.dr, m.copy)).collect::<Vec<_>>(), vec![(1, true), (2, false), (3, true)]);
        // Two moves of one piece: the second names nothing.
        assert_eq!(resolve(&fingerprints(&c, &s), &[mv(1, false), mv(2, false)]).1, vec![Some(0), None]);
        // A copy put on the piece itself, or deleted, does nothing.
        assert!(PieceMove { delete: true, ..mv(1, true) }.is_noop() && mv(0, true).is_noop());
    }
}
