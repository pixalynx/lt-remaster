//! The whole overworld as one 3D scene (screen 09, 3D mode, "Overworld"
//! scope): every grid chunk's polygons as compact geometry placed on the
//! 36×18 brick grid, and the top surface along each chunk edge for the seam
//! overlay. No textures: far chunks are draped with the composed overview
//! (`worldmap::overview`, planar UV from world x/z) and near chunks load
//! their full `render3d_chunk` meshes (`mesh::encode`).
//!
//! World placement (PRD 09 §4.1): chunk `id` at grid (gx, gy) =
//! [`world::grid_cell`] has its origin at `wx = 2048·gx`, `wz = −4096·gy`; a
//! vertex's world position is the origin plus its chunk-local x and z (y
//! unchanged, Y down). +x is east, +z north, so the overview image's pixel
//! (u, v) at `k` px per chunk is world `(u·4096/k − 2048, 4096 − v·4096/k)`.
//!
//! ```text
//! "LTOW" header (32 B)  u32 magic, u16 version 1, u16 chunk count,
//!                       u32 vertices, u32 indices (u16 units),
//!                       u16 edge samples, u16 edge step (units), u32 0 × 3
//! chunks  n × 48 B      u16 id, u8 gx, u8 gy, i32 origin x, i32 origin z,
//!                       u32 first vertex, u32 vertex count, u32 first index,
//!                       u32 quads, u32 tris,
//!                       s16 bbox[6] (local min x y z, max x y z), s16 ground y, u16 flags
//! vertices  n × 8 B     s16 x, y, z (chunk-local, Y down), u16 chunk id
//! indices   u16         per chunk from its first index: quads × 4 (strip
//!                       order: triangles 0-1-2 and 1-3-2), then tris × 3;
//!                       chunk-local vertex numbers; padded to 4 B at the end
//! edges     s16         per chunk: N, E, S, W × samples, top-surface y;
//!                       N (z = +2048) and S (z = −2048) run west → east,
//!                       E (x = +2048) and W (x = −2048) run south → north;
//!                       0x8000 = no polygon there
//! ```
//!
//! `flags` bit 0: the ground y was measured on the mesh at the chunk centre
//! (else it is the median vertex y). Vertices are only those the polygons
//! use, renumbered per chunk, so every index fits a u16.

use crate::world::{self, GRID_CHUNKS};

pub const LOD_MAGIC: u32 = u32::from_le_bytes(*b"LTOW");
pub const HEADER_BYTES: usize = 32;
pub const CHUNK_RECORD_BYTES: usize = 48;
/// Chunk extent is ±0x800 on x and z.
pub const HALF: i32 = 2048;
/// Samples per edge (every [`EDGE_STEP`] units, both corners included).
pub const EDGE_SAMPLES: usize = 129;
pub const EDGE_STEP: i32 = 32;
/// "No polygon covers this sample."
pub const NO_SURFACE: i16 = i16::MIN;

/// Edge order in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// z = +2048, west → east.
    North = 0,
    /// x = +2048, south → north.
    East = 1,
    /// z = −2048, west → east.
    South = 2,
    /// x = −2048, south → north.
    West = 3,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::North, Edge::East, Edge::South, Edge::West];

    /// Chunk-local (x, z) of sample `i`.
    pub fn point(self, i: usize) -> (i32, i32) {
        let t = -HALF + i as i32 * EDGE_STEP;
        match self {
            Edge::North => (t, HALF),
            Edge::South => (t, -HALF),
            Edge::East => (HALF, t),
            Edge::West => (-HALF, t),
        }
    }
}

/// World origin of a grid chunk: `(2048·gx, −4096·gy)`.
pub fn origin(id: u32) -> Option<(i32, i32)> {
    world::grid_cell(id).map(|(gx, gy)| (2048 * gx as i32, -4096 * gy as i32))
}

/// One grid chunk's compact geometry.
#[derive(Debug, Clone, Default)]
pub struct ChunkLod {
    pub id: u32,
    /// Used vertices, chunk-local (Y down).
    pub verts: Vec<[i16; 3]>,
    /// Quads in strip order (triangles 0-1-2, 1-3-2).
    pub quads: Vec<[u16; 4]>,
    pub tris: Vec<[u16; 3]>,
    /// Local min x, y, z, max x, y, z of the used vertices (zeros when empty).
    pub bbox: [i16; 6],
    /// Ground height for labels and framing (Y down).
    pub ground: i16,
    /// `ground` was measured on the mesh at (0, 0).
    pub ground_measured: bool,
    /// Top-surface y along N, E, S, W ([`Edge`] order), [`EDGE_SAMPLES`] each.
    pub edges: [Vec<i16>; 4],
}

impl ChunkLod {
    pub fn polygons(&self) -> usize {
        self.quads.len() + self.tris.len()
    }

    /// The chunk's triangles as local vertex triples.
    pub fn triangles(&self) -> impl Iterator<Item = [u16; 3]> + '_ {
        self.quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[1], q[3], q[2]]]).chain(self.tris.iter().copied())
    }
}

/// Parses a chunk into its LOD geometry and edge profiles. `None` when the
/// body does not parse (the payload then carries an empty record).
pub fn chunk_lod(id: u32, chunk: &[u8]) -> Option<ChunkLod> {
    let m = world::mesh(chunk)?;
    let mut remap = vec![u16::MAX; m.pool.len()];
    let mut out = ChunkLod { id, ..Default::default() };
    let mut local = |i: u16, verts: &mut Vec<[i16; 3]>| -> u16 {
        let r = &mut remap[i as usize];
        if *r == u16::MAX {
            *r = verts.len() as u16;
            verts.push(m.pool[i as usize]);
        }
        *r
    };
    for p in &m.prims {
        if p.idx[..p.n as usize].iter().any(|&i| i as usize >= m.pool.len()) {
            continue;
        }
        if p.n == 4 {
            let q = [0, 1, 2, 3].map(|k| local(p.idx[k], &mut out.verts));
            out.quads.push(q);
        } else {
            let t = [0, 1, 2].map(|k| local(p.idx[k], &mut out.verts));
            out.tris.push(t);
        }
    }
    let mut bb = [i16::MAX, i16::MAX, i16::MAX, i16::MIN, i16::MIN, i16::MIN];
    for v in &out.verts {
        for a in 0..3 {
            bb[a] = bb[a].min(v[a]);
            bb[a + 3] = bb[a + 3].max(v[a]);
        }
    }
    out.bbox = if out.verts.is_empty() { [0; 6] } else { bb };
    let surface = Surface::new(&out);
    out.edges = Edge::ALL.map(|e| {
        // Only triangles that reach the edge line can cover its samples.
        let (a, b) = (e.point(0), e.point(EDGE_SAMPLES - 1));
        let near = surface.within(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        (0..EDGE_SAMPLES).map(|i| near.top_at(e.point(i)).unwrap_or(NO_SURFACE)).collect()
    });
    match surface.within(0, 0, 0, 0).top_at((0, 0)) {
        Some(y) => {
            out.ground = y;
            out.ground_measured = true;
        }
        None => {
            let mut ys: Vec<i16> = out.verts.iter().map(|v| v[1]).collect();
            ys.sort_unstable();
            out.ground = ys.get(ys.len() / 2).copied().unwrap_or(0);
        }
    }
    Some(out)
}

/// Triangles projected on the ground plane, for top-surface queries.
struct Surface {
    /// a, b, c as (x, y, z) floats plus the xz bounding box.
    tris: Vec<([[f32; 3]; 3], [f32; 4])>,
}

impl Surface {
    fn new(lod: &ChunkLod) -> Self {
        let v = |i: u16| {
            let p = lod.verts[i as usize];
            [p[0] as f32, p[1] as f32, p[2] as f32]
        };
        let tris = lod
            .triangles()
            .map(|t| {
                let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
                let bb = [a[0].min(b[0]).min(c[0]), a[2].min(b[2]).min(c[2]), a[0].max(b[0]).max(c[0]), a[2].max(b[2]).max(c[2])];
                ([a, b, c], bb)
            })
            .collect();
        Self { tris }
    }

    /// The triangles whose x/z box touches the local rectangle (inclusive).
    fn within(&self, x0: i32, z0: i32, x1: i32, z1: i32) -> Surface {
        const EPS: f32 = 0.5;
        let (x0, z0, x1, z1) = (x0 as f32 - EPS, z0 as f32 - EPS, x1 as f32 + EPS, z1 as f32 + EPS);
        let tris = self.tris.iter().filter(|(_, bb)| bb[2] >= x0 && bb[0] <= x1 && bb[3] >= z0 && bb[1] <= z1).copied().collect();
        Surface { tris }
    }

    /// The highest (smallest y, Y down) polygon surface over local (x, z),
    /// edges inclusive within half a unit. Triangles that are vertical in
    /// x/z (walls) define no height.
    fn top_at(&self, (x, z): (i32, i32)) -> Option<i16> {
        const EPS: f32 = 0.5;
        let (px, pz) = (x as f32, z as f32);
        let mut best: Option<f32> = None;
        for ([a, b, c], bb) in &self.tris {
            if px < bb[0] - EPS || px > bb[2] + EPS || pz < bb[1] - EPS || pz > bb[3] + EPS {
                continue;
            }
            let det = (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2]);
            if det.abs() < 1.0 {
                continue;
            }
            let l1 = ((px - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (pz - a[2])) / det;
            let l2 = ((b[0] - a[0]) * (pz - a[2]) - (px - a[0]) * (b[2] - a[2])) / det;
            let l0 = 1.0 - l1 - l2;
            // Tolerance in barycentric units ≈ EPS units along the longest side.
            let side = (bb[2] - bb[0]).max(bb[3] - bb[1]).max(1.0);
            let tol = EPS / side;
            if l0 < -tol || l1 < -tol || l2 < -tol {
                continue;
            }
            let y = l0 * a[1] + l1 * b[1] + l2 * c[1];
            best = Some(best.map_or(y, |b: f32| b.min(y)));
        }
        best.map(|y| y.round().clamp(-32767.0, 32767.0) as i16)
    }
}

/// Every grid chunk (0–323) from `chunk(id)`, parsed on worker threads.
/// Chunks that fail to read or parse get an empty record.
pub fn build<F>(chunk: F) -> Vec<ChunkLod>
where
    F: Fn(u32) -> Option<Vec<u8>> + Sync,
{
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let next = std::sync::atomic::AtomicU32::new(0);
    let mut out: Vec<ChunkLod> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let id = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if id >= GRID_CHUNKS {
                            break;
                        }
                        let lod = chunk(id)
                            .and_then(|c| chunk_lod(id, &c))
                            .unwrap_or_else(|| ChunkLod { id, edges: std::array::from_fn(|_| vec![NO_SURFACE; EDGE_SAMPLES]), ..Default::default() });
                        mine.push(lod);
                    }
                    mine
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    out.sort_by_key(|c| c.id);
    out
}

/// The "LTOW" payload (layout in the module docs).
pub fn encode(chunks: &[ChunkLod]) -> Vec<u8> {
    let nv: usize = chunks.iter().map(|c| c.verts.len()).sum();
    let ni: usize = chunks.iter().map(|c| c.quads.len() * 4 + c.tris.len() * 3).sum();
    let ni_padded = ni.div_ceil(2) * 2;
    let mut out = Vec::with_capacity(HEADER_BYTES + chunks.len() * (CHUNK_RECORD_BYTES + 4 * EDGE_SAMPLES * 2) + nv * 8 + ni_padded * 2);
    let u16s = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    let u32s = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    u32s(&mut out, LOD_MAGIC);
    u16s(&mut out, 1);
    u16s(&mut out, chunks.len() as u16);
    u32s(&mut out, nv as u32);
    u32s(&mut out, ni as u32);
    u16s(&mut out, EDGE_SAMPLES as u16);
    u16s(&mut out, EDGE_STEP as u16);
    for _ in 0..3 {
        u32s(&mut out, 0);
    }
    debug_assert_eq!(out.len(), HEADER_BYTES);
    let (mut first_v, mut first_i) = (0u32, 0u32);
    for c in chunks {
        let (gx, gy) = world::grid_cell(c.id).unwrap_or((0, 0));
        let (ox, oz) = origin(c.id).unwrap_or((0, 0));
        u16s(&mut out, c.id as u16);
        out.extend_from_slice(&[gx as u8, gy as u8]);
        out.extend_from_slice(&ox.to_le_bytes());
        out.extend_from_slice(&oz.to_le_bytes());
        u32s(&mut out, first_v);
        u32s(&mut out, c.verts.len() as u32);
        u32s(&mut out, first_i);
        u32s(&mut out, c.quads.len() as u32);
        u32s(&mut out, c.tris.len() as u32);
        for b in c.bbox {
            out.extend_from_slice(&b.to_le_bytes());
        }
        out.extend_from_slice(&c.ground.to_le_bytes());
        u16s(&mut out, c.ground_measured as u16);
        first_v += c.verts.len() as u32;
        first_i += (c.quads.len() * 4 + c.tris.len() * 3) as u32;
    }
    debug_assert_eq!(out.len(), HEADER_BYTES + chunks.len() * CHUNK_RECORD_BYTES);
    for c in chunks {
        for v in &c.verts {
            for k in v {
                out.extend_from_slice(&k.to_le_bytes());
            }
            u16s(&mut out, c.id as u16);
        }
    }
    for c in chunks {
        for q in &c.quads {
            for &i in q {
                u16s(&mut out, i);
            }
        }
        for t in &c.tris {
            for &i in t {
                u16s(&mut out, i);
            }
        }
    }
    if ni_padded != ni {
        u16s(&mut out, 0);
    }
    for c in chunks {
        for e in &c.edges {
            for i in 0..EDGE_SAMPLES {
                out.extend_from_slice(&e.get(i).copied().unwrap_or(NO_SURFACE).to_le_bytes());
            }
        }
    }
    out
}
