//! The world archive (`world`, ext `F.BIN`): 524 fixed-size chunks of 0x3E800
//! bytes at LBA 20011 + n·125. Layout: `docs/research/03-world-maps-events.md`
//! §1.1–1.2.
//!
//! ```text
//! 0x00000  body (→ RAM slot 0x80010000 + (n & 3)·0x2E000)
//!          u32 groupCount; groupCount × {u32 meshOfs, u32 poolOfs, s16 cx, cy, cz, u16 0}
//!          vertex pool {s16 x, y, z, pad}; per group 8 lists F3 FT3 F4 FT4 G3 GT3 G4 GT4
//! 0x2A800  heightmap A: u32[128] row offsets (u16 units into 0x2AA00), s16 RLE
//! 0x2D800  layer B (zone ids): u32[128] row offsets (bytes into 0x2DA00, indexed 127 − z), u8 pairs
//! 0x2E000  TIM (≤ 0x10800)
//! ```
//!
//! Also here: the full primitive lists ([`mesh`]), the layers' RLE sizes, the
//! `ChunkTextureSetIds` lookup (`fe/k`) and the OVR02 chunk → NPC pack table
//! with its flag overrides ([`pack_for`]). Rendering is in `raster`/`worldmap`.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::container::{le16, le32, le_i16};
use crate::tim::{Tim, TimInfo};

pub mod walk;

pub const CHUNK_BYTES: usize = 0x3E800;
pub const CHUNK_SECTORS: u32 = 125;
pub const CHUNK_COUNT: u32 = 524;
pub const GRID_W: u32 = 36;
pub const TIM_OFFSET: usize = 0x2E000;
pub const MESH_BUDGET: u32 = 0x2A800;
pub const TIM_BUDGET: u32 = 0x10800;
/// RLE data room of layer A (0x2AA00..0x2D800) and layer B (0x2DA00..0x2E000).
pub const LAYER_A_BUDGET: u32 = 0x2E00;
pub const LAYER_B_BUDGET: u32 = 0x600;
pub(crate) const HEIGHT_TABLE: usize = 0x2A800;
pub(crate) const HEIGHT_DATA: usize = 0x2AA00;
pub(crate) const ATTR_TABLE: usize = 0x2D800;
pub(crate) const ATTR_DATA: usize = 0x2DA00;
/// Dwords per record for F3 FT3 F4 FT4 G3 GT3 G4 GT4.
pub const LIST_DWORDS: [usize; 8] = [3, 6, 3, 7, 5, 8, 6, 10];
pub const LIST_NAMES: [&str; 8] = ["F3", "FT3", "F4", "FT4", "G3", "GT3", "G4", "GT4"];

/// New areas: chunk ids past the end of the retail disc (research 03 §10).
/// `Load_WorldChunk_StateMachine` reads chunk `id` at LBA 20011 + id·125 with
/// no bound; 1328 is the first id past the retail end (LBA 186,011) and starts
/// an aligned group of four; 2499 ends the last whole group that fits a
/// 74-minute disc (333,000 sectors) with its postgap. They play as interiors.
pub const NEW_AREA_FIRST: u32 = 1328;
pub const NEW_AREA_LAST: u32 = 2499;

pub fn is_new_area(id: u32) -> bool {
    (NEW_AREA_FIRST..=NEW_AREA_LAST).contains(&id)
}

/// A chunk id the tools accept: retail (0–523) or a new area.
pub fn valid_chunk(id: u32) -> bool {
    id < CHUNK_COUNT || is_new_area(id)
}

/// Overworld grid cell of a chunk id (ids 0–323): `x = id % 36`,
/// `y = (id / 36)·2 + (id & 1)`. Interiors (324–523) have no cell.
pub fn grid_cell(id: u32) -> Option<(u32, u32)> {
    (id < 324).then(|| (id % GRID_W, (id / GRID_W) * 2 + (id & 1)))
}

/// Inverse of [`grid_cell`]; x and y must have equal parity.
pub fn chunk_at(x: u32, y: u32) -> Option<u32> {
    (x < GRID_W && y < 18 && x % 2 == y % 2).then(|| (y / 2) * GRID_W + x)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkStats {
    pub id: u32,
    pub interior: bool,
    pub grid: Option<(u32, u32)>,
    pub groups: u32,
    pub vertices: u32,
    /// Polygon count per list, in F3 … GT4 order.
    pub lists: [u32; 8],
    pub polygons: u32,
    /// Bytes of the body used by mesh data (budget 0x2A800).
    pub mesh_bytes: u32,
    /// Distinct height values, including −1 (blocked).
    pub heights: Vec<i16>,
    pub zones: Vec<u8>,
    /// TIM bytes (budget 0x10800).
    pub texture_bytes: u32,
    pub blocked_cells: u32,
    pub texture: Option<TimInfo>,
}

/// Parses one 0x3E800-byte chunk.
pub fn stats(id: u32, c: &[u8]) -> Option<ChunkStats> {
    if c.len() < CHUNK_BYTES {
        return None;
    }
    let groups = le32(c, 0);
    if groups > 256 {
        return None;
    }
    let mut lists = [0u32; 8];
    let mut mesh_end = 4 + groups as usize * 16;
    let mut pool_ofs = usize::MAX;
    let mut first_mesh = usize::MAX;
    for g in 0..groups as usize {
        let h = 4 + g * 16;
        let (mesh, pool) = (le32(c, h) as usize, le32(c, h + 4) as usize);
        pool_ofs = pool_ofs.min(pool);
        first_mesh = first_mesh.min(mesh);
        let mut p = mesh;
        for (k, dw) in LIST_DWORDS.iter().enumerate() {
            let n = le32(c.get(p..p + 4)?, 0);
            lists[k] += n;
            p += 4 + n as usize * dw * 4;
        }
        mesh_end = mesh_end.max(p);
    }
    let vertices = if groups > 0 && first_mesh > pool_ofs { ((first_mesh - pool_ofs) / 8) as u32 } else { 0 };
    let heights = heightmap(c);
    let zones = attributes(c);
    let distinct_heights: BTreeSet<i16> = heights.iter().copied().collect();
    let distinct_zones: BTreeSet<u8> = zones.iter().copied().collect();
    Some(ChunkStats {
        id,
        interior: id >= 324,
        grid: grid_cell(id),
        groups,
        vertices,
        polygons: lists.iter().sum(),
        lists,
        mesh_bytes: mesh_end as u32,
        blocked_cells: heights.iter().filter(|&&h| h == -1).count() as u32,
        heights: distinct_heights.into_iter().collect(),
        zones: distinct_zones.into_iter().collect(),
        texture_bytes: Tim::parse(&c[TIM_OFFSET..]).map(|t| t.info.len as u32).unwrap_or(0),
        texture: Tim::parse(&c[TIM_OFFSET..]).map(|t| t.info),
    })
}

/// Layer A: 128×128 walk heights (row-major, z then x). −1 = blocked.
pub fn heightmap(c: &[u8]) -> Vec<i16> {
    let mut out = Vec::with_capacity(128 * 128);
    for z in 0..128 {
        let mut p = HEIGHT_DATA + le32(c, HEIGHT_TABLE + z * 4) as usize * 2;
        let mut row = Vec::with_capacity(128);
        while row.len() < 128 && p + 2 <= c.len() {
            let tok = le16(c, p) as i16;
            p += 2;
            if tok > 0 {
                let h = le16(c, p) as i16;
                p += 2;
                row.extend(std::iter::repeat_n(h, tok as usize + 1));
            } else {
                row.push(tok);
            }
        }
        row.resize(128, -1);
        out.extend_from_slice(&row[..128]);
    }
    out
}

/// Layer B: 128×128 zone/area ids (row-major, z then x). The row table is
/// indexed by `127 − z`.
pub fn attributes(c: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 128 * 128];
    for z in 0..128 {
        let mut p = ATTR_DATA + le32(c, ATTR_TABLE + (127 - z) * 4) as usize;
        let mut x = 0usize;
        while x < 128 && p + 2 <= c.len() {
            let (run, attr) = (c[p] as usize + 1, c[p + 1]);
            p += 2;
            for _ in 0..run.min(128 - x) {
                out[z * 128 + x] = attr;
                x += 1;
            }
        }
    }
    out
}

/// Bytes of RLE data the two layers use: layer A from 0x2AA00 (cap 0x2E00),
/// layer B from 0x2DA00 (cap 0x600). Rows may share data, so this is the end of
/// the furthest row.
pub fn layer_bytes(c: &[u8]) -> (u32, u32) {
    let mut end_a = HEIGHT_DATA;
    for z in 0..128 {
        let mut p = HEIGHT_DATA + le32(c, HEIGHT_TABLE + z * 4) as usize * 2;
        let mut n = 0usize;
        while n < 128 && p + 2 <= c.len() {
            let tok = le16(c, p) as i16;
            p += 2;
            if tok > 0 {
                p += 2;
                n += tok as usize + 1;
            } else {
                n += 1;
            }
        }
        end_a = end_a.max(p);
    }
    let mut end_b = ATTR_DATA;
    for z in 0..128 {
        let mut p = ATTR_DATA + le32(c, ATTR_TABLE + z * 4) as usize;
        let mut n = 0usize;
        while n < 128 && p + 2 <= c.len() {
            n += c[p] as usize + 1;
            p += 2;
        }
        end_b = end_b.max(p);
    }
    ((end_a - HEIGHT_DATA) as u32, (end_b - ATTR_DATA) as u32)
}

/* ───────────────────────── Layout ───────────────────────── */

pub const GRID_CHUNKS: u32 = 324;
pub const GRID_H: u32 = 18;

/// Interiors stream as aligned groups of four (`id & !3`) into slots 0–3.
pub fn interior_group(id: u32) -> [u32; 4] {
    let b = id & !3;
    [b, b + 1, b + 2, b + 3]
}

/* ───────────────────────── Mesh ───────────────────────── */

/// One polygon record. Words per list (`rgbc = r | g<<8 | b<<16 | code<<24`,
/// `uv = u | v<<8`, indices u16 into the shared pool):
///
/// ```text
/// F3  rgbc, i0|i1, i2                               FT3 uv0|clut, uv1|tpage, uv2, rgbc, i0|i1, i2
/// F4  rgbc, i0|i1, i2|i3                            FT4 uv0|clut, uv1|tpage, uv2, uv3, rgbc, i0|i1, i2|i3
/// G3  rgb0c, rgb1, rgb2, i0|i1, i2                  GT3 uv0|clut, uv1|tpage, uv2, rgb0c, rgb1, rgb2, i0|i1, i2
/// G4  rgb0c..rgb3, i0|i1, i2|i3                     GT4 uv0|clut, uv1|tpage, uv2, uv3, rgb0c..rgb3, i0|i1, i2|i3
/// ```
/// Quads are in PSX strip order (triangles 0-1-2 and 1-3-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prim {
    /// 0..7 = F3 FT3 F4 FT4 G3 GT3 G4 GT4.
    pub list: u8,
    /// GPU code byte: bit 1 semi-transparent, bit 0 raw texture (no modulation).
    pub code: u8,
    /// 3 or 4 vertices.
    pub n: u8,
    pub idx: [u16; 4],
    /// Per-vertex colour (flat records repeat the first), 128 = 1.0 when textured.
    pub rgb: [[u8; 3]; 4],
    pub uv: [[u8; 2]; 4],
    pub clut: u16,
    pub tpage: u16,
}

impl Prim {
    pub fn textured(&self) -> bool {
        self.list & 1 == 1
    }
    pub fn gouraud(&self) -> bool {
        self.list >= 4
    }
    pub fn semi(&self) -> bool {
        self.code & 2 != 0
    }
    pub fn raw(&self) -> bool {
        self.code & 1 != 0
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshGroup {
    pub index: u32,
    /// Body-relative offset of the group's 8 lists.
    pub mesh_ofs: u32,
    /// Culling centre (no transform), chunk-local, Y down.
    pub centre: [i16; 3],
    /// Polygons per list, F3 … GT4.
    pub lists: [u32; 8],
    /// Index range into [`Mesh::prims`].
    pub first: u32,
    pub count: u32,
    /// Bounding box of the referenced vertices: min x,y,z, max x,y,z.
    pub bbox: [i16; 6],
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub groups: Vec<MeshGroup>,
    /// Vertex pool, chunk-local `[x, y, z]` (Y down).
    pub pool: Vec<[i16; 3]>,
    pub prims: Vec<Prim>,
    /// End of the last list (the mesh budget is 0x2A800).
    pub mesh_end: u32,
}

/// Parses every group's 8 primitive lists and the shared vertex pool
/// (`chunk_tool.parse_body`). `None` when the header is implausible.
pub fn mesh(c: &[u8]) -> Option<Mesh> {
    let body = c.get(..HEIGHT_TABLE)?;
    let count = le32(body, 0) as usize;
    if count > 400 || 4 + count * 16 > body.len() {
        return None;
    }
    let mut groups = Vec::with_capacity(count);
    let mut prims = Vec::new();
    let mut mesh_end = 4 + count * 16;
    let mut pool_ofs = usize::MAX;
    let mut first_mesh = usize::MAX;
    let mut max_idx = 0usize;
    for g in 0..count {
        let h = 4 + g * 16;
        let (mesh_ofs, pool) = (le32(body, h) as usize, le32(body, h + 4) as usize);
        let centre = [le_i16(body, h + 8), le_i16(body, h + 10), le_i16(body, h + 12)];
        pool_ofs = pool_ofs.min(pool);
        first_mesh = first_mesh.min(mesh_ofs);
        let first = prims.len();
        let mut lists = [0u32; 8];
        let mut p = mesh_ofs;
        for (k, &dw) in LIST_DWORDS.iter().enumerate() {
            let n = le32(body.get(p..p + 4)?, 0) as usize;
            p += 4;
            if p + n * dw * 4 > body.len() {
                return None;
            }
            lists[k] = n as u32;
            for j in 0..n {
                let w = |i: usize| le32(body, p + (j * dw + i) * 4);
                let prim = record(k as u8, &w);
                max_idx = max_idx.max(prim.idx[..prim.n as usize].iter().copied().max().unwrap_or(0) as usize);
                prims.push(prim);
            }
            p += n * dw * 4;
        }
        mesh_end = mesh_end.max(p);
        groups.push(MeshGroup {
            index: g as u32,
            mesh_ofs: mesh_ofs as u32,
            centre,
            lists,
            first: first as u32,
            count: (prims.len() - first) as u32,
            bbox: [0; 6],
        });
    }
    let pool_len = if count > 0 && first_mesh > pool_ofs { (first_mesh - pool_ofs) / 8 } else { 0 };
    let pool_len = pool_len.max(if prims.is_empty() { 0 } else { max_idx + 1 });
    let pool: Vec<[i16; 3]> = (0..pool_len)
        .map(|k| {
            let o = pool_ofs + k * 8;
            if o + 6 <= body.len() {
                [le_i16(body, o), le_i16(body, o + 2), le_i16(body, o + 4)]
            } else {
                [0, 0, 0]
            }
        })
        .collect();
    for g in &mut groups {
        let mut bb = [i16::MAX, i16::MAX, i16::MAX, i16::MIN, i16::MIN, i16::MIN];
        for p in &prims[g.first as usize..(g.first + g.count) as usize] {
            for &i in &p.idx[..p.n as usize] {
                let v = pool[i as usize];
                for a in 0..3 {
                    bb[a] = bb[a].min(v[a]);
                    bb[a + 3] = bb[a + 3].max(v[a]);
                }
            }
        }
        if g.count > 0 {
            g.bbox = bb;
        }
    }
    Some(Mesh { groups, pool, prims, mesh_end: mesh_end as u32 })
}

/// One group's eight primitive lists (F3 … GT4, [`LIST_DWORDS`] a record) at `at` in `body`: its
/// primitives, the count per list and where the lists end. A chunk group's layout; OVR24's sky
/// cells (the first flight, `func_80133914`) hold theirs the same way. `None` when they run past
/// `body`.
pub fn lists(body: &[u8], at: usize) -> Option<(Vec<Prim>, [u32; 8], usize)> {
    let mut prims = Vec::new();
    let mut counts = [0u32; 8];
    let mut p = at;
    for (k, &dw) in LIST_DWORDS.iter().enumerate() {
        let n = le32(body.get(p..p + 4)?, 0) as usize;
        p += 4;
        if p + n * dw * 4 > body.len() {
            return None;
        }
        counts[k] = n as u32;
        for j in 0..n {
            let w = |i: usize| le32(body, p + (j * dw + i) * 4);
            prims.push(record(k as u8, &w));
        }
        p += n * dw * 4;
    }
    Some((prims, counts, p))
}

fn record(list: u8, w: &dyn Fn(usize) -> u32) -> Prim {
    let lo = |v: u32| (v & 0xFFFF) as u16;
    let hi = |v: u32| (v >> 16) as u16;
    let rgb = |v: u32| [v as u8, (v >> 8) as u8, (v >> 16) as u8];
    let uv = |v: u32| [v as u8, (v >> 8) as u8];
    let n: u8 = if matches!(list, 2 | 3 | 6 | 7) { 4 } else { 3 };
    let textured = list & 1 == 1;
    let tex = if textured { n as usize } else { 0 };
    // Textured records start with the UV words (+ CLUT, tpage in the high halves).
    let colours = if list >= 4 { n as usize } else { 1 };
    let c0 = tex;
    let i0 = c0 + colours;
    let mut p = Prim { list, code: (w(c0) >> 24) as u8, n, idx: [0; 4], rgb: [[0; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
    for k in 0..n as usize {
        p.rgb[k] = rgb(w(c0 + if colours > 1 { k } else { 0 }));
        if textured {
            p.uv[k] = uv(w(k));
        }
    }
    if textured {
        p.clut = hi(w(0));
        p.tpage = hi(w(1));
    }
    let (a, b) = (w(i0), w(i0 + 1));
    p.idx = [lo(a), hi(a), lo(b), if n == 4 { hi(b) } else { 0 }];
    p
}

/* ───────────────────────── Shared texture sources ───────────────────────── */

/// `ChunkTextureSetIds` (EXE 0x801B7108): 56 s16 chunk ids. A chunk at index i
/// loads `fe/(i + 1)`; every other chunk loads `fe/0`
/// (`Field_ChunkStreamTask`, `main_801897ac.c:628`).
pub const TEXSET_IDS_VADDR: u32 = 0x801B_7108;
pub const TEXSET_IDS_LEN: usize = 0x38;

pub fn texset_ids(exe: &crate::PsxExe) -> crate::Result<Vec<u16>> {
    let b = exe.slice(exe.va(TEXSET_IDS_VADDR), TEXSET_IDS_LEN * 2)?;
    Ok((0..TEXSET_IDS_LEN).map(|i| le16(b, i * 2)).collect())
}

/// `fe/k` region texture set for a chunk.
pub fn texset_for(ids: &[u16], chunk: u32) -> u32 {
    ids.iter().position(|&c| c as u32 == chunk).map(|i| i as u32 + 1).unwrap_or(0)
}

/* ───────────────────────── Chunk → NPC pack ───────────────────────── */

/// `D_801FC4A4` in `ovr/2` (load address 0x801DF000): `{s16 chunk, s16 pack}`
/// pairs terminated by chunk 0; the first match wins.
pub const PACK_TABLE_OFFSET: usize = 0x1D4A4;
const PACK_TABLE_VADDR: u32 = 0x801F_C4A4;

/// Reads the chunk → pack pairs from the USA `ovr/2` file.
pub fn pack_table(ovr2: &[u8]) -> Vec<(u16, u16)> {
    pack_table_at(ovr2, PACK_TABLE_OFFSET)
}

/// [`pack_table`] for the release `exe` belongs to (its OVR02 keeps the table elsewhere).
pub fn pack_table_in(exe: &crate::PsxExe, ovr2: &[u8]) -> Vec<(u16, u16)> {
    pack_table_at(ovr2, (exe.ovr_va(2, PACK_TABLE_VADDR) - crate::gamedata::OVR_LOAD) as usize)
}

fn pack_table_at(ovr2: &[u8], at: usize) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut p = at;
    while p + 4 <= ovr2.len() && out.len() < 1024 {
        let chunk = le16(ovr2, p);
        if chunk == 0 {
            break;
        }
        out.push((chunk, le16(ovr2, p + 2)));
        p += 4;
    }
    out
}

/// Port of `Field_LookupChunkNpcBinId` (`src/ovr02/ovr02.c:3254`) for a flag
/// state. `None` = no table entry (the current pack stays loaded).
pub fn pack_for(table: &[(u16, u16)], chunk: u32, flag: &dyn Fn(u16) -> bool) -> Option<u32> {
    let &(_, pack) = table.iter().find(|(c, _)| *c as u32 == chunk)?;
    let mut value = pack as i32;
    match value {
        0x0A if flag(0xE6) => value = 0x27,
        0x16 if flag(0xBA) => value = 0x19,
        0x16 if flag(0xE6) => value = 0x15,
        _ => {}
    }
    if chunk == 0xBC && !flag(0xBA) {
        value = -1;
    }
    if flag(0xF7) && value != 4 && value != 6 {
        value = -1;
    }
    (value >= 0).then_some(value as u32)
}

#[cfg(test)]
mod lists_tests {
    use super::*;

    /// The eight lists of a group: one flat triangle, one Gouraud quad, the rest empty.
    #[test]
    fn a_groups_lists_decode() {
        let mut b = Vec::new();
        let mut put = |w: u32| b.extend_from_slice(&w.to_le_bytes());
        put(1); // F3
        put(0x2010_2030); // code 0x20, rgb 30 20 10
        put(0x0002_0001); // indices 1, 2
        put(0x0000_0003); // index 3
        for _ in 0..5 {
            put(0); // FT3 F4 FT4 G3 GT3
        }
        put(1); // G4
        for c in [0x3800_0101u32, 0x0002_0202, 0x0003_0303, 0x0004_0404] {
            put(c);
        }
        put(0x0001_0000); // indices 0, 1
        put(0x0003_0002); // indices 2, 3
        put(0); // GT4
        let (prims, counts, end) = lists(&b, 0).unwrap();
        assert_eq!(counts, [1, 0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(end, b.len());
        assert_eq!((prims[0].n, prims[0].code, prims[0].rgb[0], &prims[0].idx[..3]), (3, 0x20, [0x30, 0x20, 0x10], &[1u16, 2, 3][..]));
        assert_eq!((prims[1].n, prims[1].code, prims[1].rgb[3], prims[1].idx), (4, 0x38, [4, 4, 4], [0, 1, 2, 3]));
        assert!(lists(&b[..b.len() - 4], 0).is_none(), "cut short");
    }
}
