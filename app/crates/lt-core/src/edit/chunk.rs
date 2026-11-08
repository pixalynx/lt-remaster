//! World chunks (screen 10, PRD 10 §5.1) → `chunk/n`. [`Chunk`] is the whole
//! 0x3E800-byte chunk in editable form: draw groups of polygons over one
//! shared vertex pool, the two 128×128 layers and the texture page.
//! [`Chunk::serialize`] lays them out again the way Tamsoft's level tool did,
//! so every retail chunk parses and serializes back byte for byte
//! (`tests/chunks.rs`, all 524).
//!
//! The layout is in [`crate::world`]. The encoder relies on what every retail
//! chunk does:
//! - the body is the header, the pool, then each group's 8 lists in group
//!   order, with no gaps, and every group points at the same pool;
//! - the pool holds exactly the vertices the polygons use, pad halfwords 0;
//! - a record's code byte is its list's GPU code plus the semi-transparent
//!   and raw bits, and no spare bit is set (uv2/uv3 high halves, colour pad
//!   bytes, the high half of a triangle's last index word);
//! - a layer row reuses the previous row's data only when it is identical.
//!
//! Stale bytes the tool left after each section are kept from the chunk that
//! was parsed (zeros for [`Chunk::blank`]); the game never reads them.

use serde::{Deserialize, Serialize};

use crate::container::{le16, le32};
use crate::tim::{rect_at, VramRect};
use crate::world::{
    self, Prim, ATTR_DATA, ATTR_TABLE, CHUNK_BYTES, HEIGHT_DATA, HEIGHT_TABLE, LAYER_A_BUDGET, LAYER_B_BUDGET, LIST_DWORDS, MESH_BUDGET,
    TIM_BUDGET, TIM_OFFSET,
};

use super::{EditError, Result};

/// Cells per layer (128 × 128, row-major, z then x).
pub const CELLS: usize = 128 * 128;
/// Indices are u16.
pub const MAX_VERTICES: usize = 0x10000;
/// More than any retail chunk (77) and what [`world::stats`] still reads.
pub const MAX_GROUPS: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Culling centre, chunk-local, Y down. The game only culls by it, so keep
    /// it in the middle of the group ([`Chunk::recompute_centre`]).
    pub centre: [i16; 3],
    /// Written list by list (F3 … GT4), in this order within a list.
    pub prims: Vec<Prim>,
}

/// The chunk's own TIM. Tamsoft writes flags 0x0A on 4bpp pages; the CLUT
/// decides the depth (see [`crate::tim`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Texture {
    pub flags: u32,
    /// CLUT rectangle and its BGR555 entries, row-major.
    pub clut: Option<(VramRect, Vec<u16>)>,
    /// Pixel rectangle (width in VRAM halfwords) and its bytes; at 4bpp two
    /// pixels per byte, low nibble first.
    pub image: VramRect,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub groups: Vec<Group>,
    /// Chunk-local `[x, y, z]`, Y down.
    pub pool: Vec<[i16; 3]>,
    /// Layer A walk heights, [`CELLS`] long. −1 = blocked.
    pub heights: Vec<i16>,
    /// Layer B zone ids, [`CELLS`] long.
    pub zones: Vec<u8>,
    pub texture: Texture,
    /// What [`Chunk::serialize`] writes over.
    base: Vec<u8>,
}

/// Encoded sizes against the budgets, for the editor's meters. Over-budget
/// sizes are reported too; [`Chunk::serialize`] refuses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sizes {
    /// Header + pool + lists (budget [`MESH_BUDGET`]).
    pub mesh: u32,
    pub vertices: u32,
    pub polygons: u32,
    /// RLE data bytes (budgets [`LAYER_A_BUDGET`], [`LAYER_B_BUDGET`]).
    pub layer_a: u32,
    pub layer_b: u32,
    /// TIM bytes (budget [`TIM_BUDGET`]).
    pub texture: u32,
}

impl Chunk {
    /// Parses a whole chunk file (`chunk/n`, exactly 0x3E800 bytes).
    pub fn parse(c: &[u8]) -> Result<Self> {
        if c.len() != CHUNK_BYTES {
            return Err(EditError::new("CHUNK_SIZE", format!("A chunk is {CHUNK_BYTES:#x} bytes; this file is {:#x}.", c.len())));
        }
        let m = world::mesh(c).ok_or_else(|| EditError::new("CHUNK_BAD_MESH", "The chunk's draw groups don't parse."))?;
        let groups = m
            .groups
            .iter()
            .map(|g| Group { centre: g.centre, prims: m.prims[g.first as usize..(g.first + g.count) as usize].to_vec() })
            .collect();
        let texture = Texture::parse(&c[TIM_OFFSET..]).ok_or_else(|| EditError::new("CHUNK_BAD_TIM", format!("No texture page (TIM) at {TIM_OFFSET:#x}.")))?;
        Ok(Self { groups, pool: m.pool, heights: world::heightmap(c), zones: world::attributes(c), texture, base: c.to_vec() })
    }

    /// An empty chunk for id `id`: no polygons, flat height 0, zone 0, and a
    /// blank 4bpp 256×256 page with 16 CLUTs where the game puts slot
    /// `id & 3` (page x = 640 + slot·64, CLUTs at (32 + slot·32, 480)).
    pub fn blank(id: u32) -> Self {
        let slot = (id & 3) as u16;
        Self {
            groups: Vec::new(),
            pool: Vec::new(),
            heights: vec![0; CELLS],
            zones: vec![0; CELLS],
            texture: Texture {
                flags: 0x0A,
                clut: Some((VramRect { x: 32 + slot * 32, y: 480, w: 16, h: 16 }, vec![0; 16 * 16])),
                image: VramRect { x: 640 + slot * 64, y: 0, w: 64, h: 256 },
                pixels: vec![0; 64 * 256 * 2],
            },
            base: vec![0; CHUNK_BYTES],
        }
    }

    /// Lays [`Chunk::serialize`] over `base` (exactly 0x3E800 bytes) instead
    /// of the parsed file. Edits pass the retail chunk, so bytes a smaller
    /// encode no longer covers go back to retail rather than keeping an
    /// earlier save's.
    pub fn rebase(&mut self, base: &[u8]) -> Result<()> {
        if base.len() != CHUNK_BYTES {
            return Err(EditError::new("CHUNK_SIZE", format!("A chunk is {CHUNK_BYTES:#x} bytes; the base is {:#x}.", base.len())));
        }
        self.base = base.to_vec();
        Ok(())
    }

    /// A new area to build on: flat untextured ground at y = −128 over the
    /// whole chunk (8 × 8 quads of 512 units, one draw group each, in the
    /// retail floor order north-west, north-east, south-west, south-east so
    /// they face up), walk height −129, and a wall (0) on the outermost ring of
    /// cells so the Knight can't leave it. Its page is the whole column
    /// ([`Chunk::full_page`]).
    pub fn new_area(id: u32) -> Self {
        let mut c = Self::blank(id);
        c.full_page();
        const N: i16 = 8;
        let at = |i: i16| -2048 + i * 512;
        for z in 0..=N {
            for x in 0..=N {
                c.pool.push([at(x), -128, at(z)]);
            }
        }
        let v = |x: i16, z: i16| (z * (N + 1) + x) as u16;
        for z in 0..N {
            for x in 0..N {
                let q = Prim {
                    list: 2,
                    code: 0x28,
                    n: 4,
                    idx: [v(x, z + 1), v(x + 1, z + 1), v(x, z), v(x + 1, z)],
                    rgb: [[84, 116, 70]; 4],
                    uv: [[0; 2]; 4],
                    clut: 0,
                    tpage: 0,
                };
                c.groups.push(Group { centre: [0; 3], prims: vec![q] });
                let g = c.groups.len() - 1;
                c.recompute_centre(g);
            }
        }
        for i in 0..CELLS {
            let (col, row) = (i % 128, i / 128);
            c.heights[i] = if col == 0 || row == 0 || col == 127 || row == 127 { 0 } else { -129 };
        }
        c
    }

    /// Grows a 256-row page that starts at row 0 to the whole 512-row column
    /// and a 16-wide CLUT block to 32 palettes, keeping its texels and
    /// palettes: the page the biggest retail chunks have (124 of them, chunk
    /// 78 among them), room for the construction kit's textures.
    pub fn full_page(&mut self) {
        let t = &mut self.texture;
        if t.image.y == 0 && t.image.h == 256 {
            t.image.h = 512;
            t.pixels.resize(t.image.w as usize * 2 * 512, 0);
        }
        if let Some((r, e)) = &mut t.clut {
            if r.w == 16 && r.y == 480 && r.h <= 16 {
                let mut wide = vec![0u16; 32 * 16];
                for row in 0..r.h as usize {
                    wide[row * 32..row * 32 + 16].copy_from_slice(&e[row * 16..row * 16 + 16]);
                }
                (r.w, r.h) = (32, 16);
                *e = wide;
            }
        }
    }

    /// The chunk file, or the first budget or consistency check it fails.    /// The chunk file, or the first budget or consistency check it fails.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let e = self.encode()?;
        let s = e.sizes(self);
        if self.groups.len() > MAX_GROUPS {
            return Err(EditError::new("CHUNK_GROUPS", format!("{} draw groups; a chunk takes at most {MAX_GROUPS}.", self.groups.len())));
        }
        if s.mesh > MESH_BUDGET {
            return Err(EditError::new(
                "CHUNK_MESH_BUDGET",
                format!("The geometry needs {} bytes; a chunk has room for {MESH_BUDGET} ({MESH_BUDGET:#x}). Remove polygons or vertices.", s.mesh),
            ));
        }
        if s.layer_a > LAYER_A_BUDGET {
            return Err(EditError::new(
                "CHUNK_LAYER_A_BUDGET",
                format!("The walk heights need {} bytes; there is room for {LAYER_A_BUDGET}. Use longer runs of one height.", s.layer_a),
            ));
        }
        if s.layer_b > LAYER_B_BUDGET {
            return Err(EditError::new(
                "CHUNK_LAYER_B_BUDGET",
                format!("The zones need {} bytes; there is room for {LAYER_B_BUDGET}. Use longer runs of one zone.", s.layer_b),
            ));
        }
        if s.texture > TIM_BUDGET {
            return Err(EditError::new(
                "CHUNK_TIM_BUDGET",
                format!("The texture page needs {} bytes; there is room for {TIM_BUDGET} ({TIM_BUDGET:#x}).", s.texture),
            ));
        }
        let mut out = self.base.clone();
        out[..e.geometry.len()].copy_from_slice(&e.geometry);
        for (table, data, (rows, rle)) in [(HEIGHT_TABLE, HEIGHT_DATA, &e.layer_a), (ATTR_TABLE, ATTR_DATA, &e.layer_b)] {
            for (k, ofs) in rows.iter().enumerate() {
                out[table + k * 4..table + k * 4 + 4].copy_from_slice(&ofs.to_le_bytes());
            }
            out[data..data + rle.len()].copy_from_slice(rle);
        }
        out[TIM_OFFSET..TIM_OFFSET + e.texture.len()].copy_from_slice(&e.texture);
        Ok(out)
    }

    /// Encoded sizes, or the consistency error that stops encoding.
    pub fn sizes(&self) -> Result<Sizes> {
        Ok(self.encode()?.sizes(self))
    }

    /// Sets group `g`'s centre to the middle of the bounding box of the
    /// vertices it uses (`gc_level.py build` rounds down the same way). An
    /// empty group keeps its centre.
    pub fn recompute_centre(&mut self, g: usize) {
        let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
        for p in &self.groups[g].prims {
            for &i in &p.idx[..corners(p.list)] {
                if let Some(v) = self.pool.get(i as usize) {
                    for a in 0..3 {
                        lo[a] = lo[a].min(v[a] as i32);
                        hi[a] = hi[a].max(v[a] as i32);
                    }
                }
            }
        }
        if lo[0] <= hi[0] {
            self.groups[g].centre = [0, 1, 2].map(|a| (lo[a] + hi[a]).div_euclid(2) as i16);
        }
    }

    /// Cells layer A can't store: a positive height on a single cell (a
    /// positive token means a run). [`Chunk::serialize`] writes them as 0.
    pub fn unencodable_heights(&self) -> Vec<usize> {
        let h = &self.heights;
        (0..h.len().min(CELLS))
            .filter(|&i| {
                let x = i % 128;
                h[i] > 0 && (x == 0 || h[i - 1] != h[i]) && (x == 127 || h.get(i + 1) != Some(&h[i]))
            })
            .collect()
    }

    fn encode(&self) -> Result<Encoded> {
        for (name, len) in [("A", self.heights.len()), ("B", self.zones.len())] {
            if len != CELLS {
                return Err(EditError::new("CHUNK_LAYER_SIZE", format!("Layer {name} has {len} cells; it must be 128 × 128 ({CELLS}).")));
            }
        }
        if self.pool.len() > MAX_VERTICES {
            return Err(EditError::new("CHUNK_VERTICES", format!("{} vertices; a chunk holds at most {MAX_VERTICES}.", self.pool.len())));
        }
        Ok(Encoded {
            geometry: self.encode_geometry()?,
            layer_a: encode_heights(&self.heights),
            layer_b: encode_zones(&self.zones),
            texture: self.texture.encode()?,
        })
    }

    fn encode_geometry(&self) -> Result<Vec<u8>> {
        let header = 4 + self.groups.len() * 16;
        let pool_end = header + self.pool.len() * 8;
        let mut meshes = Vec::new();
        let mut offsets = Vec::with_capacity(self.groups.len());
        for (g, group) in self.groups.iter().enumerate() {
            offsets.push((pool_end + meshes.len()) as u32);
            if let Some(p) = group.prims.iter().find(|p| p.list > 7) {
                return Err(EditError::new("CHUNK_BAD_PRIM", format!("Group {g} has a polygon in list {}; lists are 0–7 (F3 … GT4).", p.list)));
            }
            for list in 0..8u8 {
                let start = meshes.len();
                meshes.extend_from_slice(&[0; 4]);
                let mut n = 0u32;
                for p in group.prims.iter().filter(|p| p.list == list) {
                    if let Some(&i) = p.idx[..corners(list)].iter().find(|&&i| i as usize >= self.pool.len()) {
                        return Err(EditError::new("CHUNK_BAD_INDEX", format!("Group {g} uses vertex {i}, but the pool has {}.", self.pool.len())));
                    }
                    for w in record_words(p) {
                        meshes.extend_from_slice(&w.to_le_bytes());
                    }
                    n += 1;
                }
                meshes[start..start + 4].copy_from_slice(&n.to_le_bytes());
            }
        }
        let mut out = Vec::with_capacity(pool_end + meshes.len());
        out.extend_from_slice(&(self.groups.len() as u32).to_le_bytes());
        for (group, ofs) in self.groups.iter().zip(&offsets) {
            out.extend_from_slice(&ofs.to_le_bytes());
            out.extend_from_slice(&(header as u32).to_le_bytes());
            for c in group.centre {
                out.extend_from_slice(&c.to_le_bytes());
            }
            out.extend_from_slice(&[0; 2]);
        }
        for v in &self.pool {
            for c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
            out.extend_from_slice(&[0; 2]);
        }
        out.extend_from_slice(&meshes);
        Ok(out)
    }
}

/// One layer cell to write: `cell = row·128 + col` (row = z cell). Heights
/// take any s16 (−1 = blocked), zones 0–255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellWrite {
    pub cell: u32,
    pub value: i32,
}

/// A painted chunk file, its sizes, and the cells written as 0 because layer A
/// can't hold a single positive height ([`Chunk::unencodable_heights`]).
#[derive(Debug, Clone)]
pub struct Painted {
    pub bytes: Vec<u8>,
    pub sizes: Sizes,
    pub clamped: Vec<usize>,
}

/// Writes layer cells into `current` (`chunk/n` as the project has it): the
/// height and zone brushes of screen 10. The layout goes over `retail` (see
/// [`Chunk::rebase`]), so painting every cell back gives the retail file.
pub fn paint(retail: &[u8], current: &[u8], heights: &[CellWrite], zones: &[CellWrite]) -> Result<Painted> {
    let mut c = Chunk::parse(current)?;
    c.rebase(retail)?;
    for (writes, range, name) in [(heights, i16::MIN as i32..=i16::MAX as i32, "Height"), (zones, 0..=255, "Zone")] {
        for w in writes {
            if w.cell as usize >= CELLS {
                return Err(EditError::new("CHUNK_CELL", format!("Cell {} is outside the 128 × 128 grid.", w.cell)));
            }
            if !range.contains(&w.value) {
                return Err(EditError::new("CHUNK_CELL_VALUE", format!("{name} {} is outside {}–{}.", w.value, range.start(), range.end())));
            }
        }
    }
    for w in heights {
        c.heights[w.cell as usize] = w.value as i16;
    }
    for w in zones {
        c.zones[w.cell as usize] = w.value as u8;
    }
    let clamped = c.unencodable_heights();
    let bytes = c.serialize()?;
    Ok(Painted { bytes, sizes: c.sizes()?, clamped })
}

struct Encoded {
    geometry: Vec<u8>,
    layer_a: ([u32; 128], Vec<u8>),
    layer_b: ([u32; 128], Vec<u8>),
    texture: Vec<u8>,
}

impl Encoded {
    fn sizes(&self, c: &Chunk) -> Sizes {
        Sizes {
            mesh: self.geometry.len() as u32,
            vertices: c.pool.len() as u32,
            polygons: c.groups.iter().map(|g| g.prims.len() as u32).sum(),
            layer_a: self.layer_a.1.len() as u32,
            layer_b: self.layer_b.1.len() as u32,
            texture: self.texture.len() as u32,
        }
    }
}

/// 3 or 4 vertices for list 0..7 (F3 FT3 F4 FT4 G3 GT3 G4 GT4).
fn corners(list: u8) -> usize {
    if matches!(list, 2 | 3 | 6 | 7) {
        4
    } else {
        3
    }
}

/// The inverse of `world::record`. The code byte is the list's GPU code
/// (`0x20 | list << 2`) plus the prim's semi-transparent and raw bits.
fn record_words(p: &Prim) -> Vec<u32> {
    let list = p.list as usize;
    let n = corners(p.list);
    let code = 0x20 | (list as u32) << 2 | (p.code & 3) as u32;
    let mut w = Vec::with_capacity(LIST_DWORDS[list]);
    if p.list & 1 == 1 {
        for (k, uv) in p.uv[..n].iter().enumerate() {
            let hi = match k {
                0 => p.clut,
                1 => p.tpage,
                _ => 0,
            };
            w.push(uv[0] as u32 | (uv[1] as u32) << 8 | (hi as u32) << 16);
        }
    }
    let colours = if p.list >= 4 { n } else { 1 };
    for (k, c) in p.rgb[..colours].iter().enumerate() {
        w.push(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | if k == 0 { code << 24 } else { 0 });
    }
    let i = p.idx;
    w.push(i[0] as u32 | (i[1] as u32) << 16);
    w.push(i[2] as u32 | if n == 4 { (i[3] as u32) << 16 } else { 0 });
    w
}

/// Layer A rows: a token `n > 0` means `n + 1` cells of the next token's
/// height, a token `t <= 0` one cell of height `t`; a single positive cell
/// can't be stored and becomes 0. Row offsets are in halfwords.
fn encode_heights(h: &[i16]) -> ([u32; 128], Vec<u8>) {
    let mut rows = [0u32; 128];
    let mut data = Vec::new();
    let mut prev: Option<(Vec<u8>, u32)> = None;
    for (z, row) in h.as_chunks::<128>().0.iter().enumerate() {
        let mut blob = Vec::new();
        let mut i = 0;
        while i < 128 {
            let mut j = i;
            while j + 1 < 128 && row[j + 1] == row[i] {
                j += 1;
            }
            let toks = if j == i { vec![row[i].min(0)] } else { vec![(j - i) as i16, row[i]] };
            for t in toks {
                blob.extend_from_slice(&t.to_le_bytes());
            }
            i = j + 1;
        }
        rows[z] = reuse_or_append(&mut prev, &mut data, blob) / 2;
    }
    (rows, data)
}

/// Layer B rows: byte pairs `{run − 1, zone}`, written last row first, and
/// the row table is indexed `127 − z`. Row offsets are in bytes.
fn encode_zones(zones: &[u8]) -> ([u32; 128], Vec<u8>) {
    let mut rows = [0u32; 128];
    let mut data = Vec::new();
    let mut prev: Option<(Vec<u8>, u32)> = None;
    for z in (0..128).rev() {
        let row = &zones[z * 128..z * 128 + 128];
        let mut blob = Vec::new();
        let mut i = 0;
        while i < 128 {
            let mut j = i;
            while j + 1 < 128 && row[j + 1] == row[i] {
                j += 1;
            }
            blob.extend_from_slice(&[(j - i) as u8, row[i]]);
            i = j + 1;
        }
        rows[127 - z] = reuse_or_append(&mut prev, &mut data, blob);
    }
    (rows, data)
}

/// Tamsoft's tool shares a row's data only with the row written just before.
fn reuse_or_append(prev: &mut Option<(Vec<u8>, u32)>, data: &mut Vec<u8>, blob: Vec<u8>) -> u32 {
    if let Some((b, ofs)) = prev {
        if *b == blob {
            return *ofs;
        }
    }
    let ofs = data.len() as u32;
    data.extend_from_slice(&blob);
    *prev = Some((blob, ofs));
    ofs
}

impl Texture {
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 20 || le32(b, 0) != 0x10 {
            return None;
        }
        let flags = le32(b, 4);
        let mut p = 8;
        let clut = if flags & 8 != 0 {
            let rect = rect_at(b, p + 4);
            let n = rect.w as usize * rect.h as usize;
            let raw = b.get(p + 12..p + 12 + n * 2)?;
            p += le32(b, p) as usize;
            Some((rect, (0..n).map(|k| le16(raw, k * 2)).collect()))
        } else {
            None
        };
        let image = rect_at(b, p + 4);
        let pixels = b.get(p + 12..p + 12 + image.w as usize * image.h as usize * 2)?.to_vec();
        Some(Self { flags, clut, image, pixels })
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let shape = |what: &str, have: usize, r: &VramRect, need: usize| {
            EditError::new("CHUNK_TIM_SHAPE", format!("The texture page's {what} hold {have} entries; its {} × {} rectangle needs {need}.", r.w, r.h))
        };
        let need = self.image.w as usize * self.image.h as usize * 2;
        if self.pixels.len() != need {
            return Err(shape("pixels", self.pixels.len(), &self.image, need));
        }
        let mut out = Vec::with_capacity(8 + 12 + need);
        out.extend_from_slice(&0x10u32.to_le_bytes());
        out.extend_from_slice(&((self.flags & !8) | if self.clut.is_some() { 8 } else { 0 }).to_le_bytes());
        let mut block = |r: &VramRect, data: &[u8]| {
            out.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
            for v in [r.x, r.y, r.w, r.h] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(data);
        };
        if let Some((r, entries)) = &self.clut {
            let need = r.w as usize * r.h as usize;
            if entries.len() != need {
                return Err(shape("palettes", entries.len(), r, need));
            }
            block(r, &entries.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>());
        }
        block(&self.image, &self.pixels);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(list: u8, idx: [u16; 4]) -> Prim {
        Prim { list, code: 0, n: 4, idx, rgb: [[128, 64, 32]; 4], uv: [[1, 2], [3, 4], [5, 6], [7, 8]], clut: 0x7802, tpage: 0x000A }
    }

    #[test]
    fn a_blank_chunk_round_trips() {
        let c = Chunk::blank(330);
        let b = c.serialize().unwrap();
        assert_eq!(b.len(), CHUNK_BYTES);
        let d = Chunk::parse(&b).unwrap();
        assert!(d.groups.is_empty() && d.pool.is_empty());
        assert_eq!(d.heights, c.heights);
        assert_eq!(d.zones, c.zones);
        assert_eq!(d.texture, c.texture);
        // Slot 2: page at x 768, CLUTs at (96, 480).
        assert_eq!((d.texture.image.x, d.texture.clut.as_ref().unwrap().0.x), (768, 96));
        assert_eq!(d.serialize().unwrap(), b);
    }

    #[test]
    fn polygons_heights_and_zones_survive_a_round_trip() {
        let mut c = Chunk::blank(0);
        c.pool = vec![[-64, -128, 0], [64, -128, 0], [-64, -128, 128], [64, -128, 128], [0, -192, 64]];
        let tri = Prim { list: 4, code: 2, n: 3, idx: [0, 1, 4, 0], rgb: [[10, 20, 30], [40, 50, 60], [70, 80, 90], [0; 3]], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        // Out of list order on purpose: serialize groups them F3 … GT4.
        c.groups.push(Group { centre: [0; 3], prims: vec![quad(7, [0, 1, 2, 3]), tri, quad(3, [0, 1, 2, 3])] });
        c.recompute_centre(0);
        assert_eq!(c.groups[0].centre, [0, -160, 64]);
        for x in 10..20 {
            c.heights[5 * 128 + x] = -129;
            c.zones[(127 - 5) * 128 + x] = 68;
        }
        c.heights[7 * 128] = -1;
        let b = c.serialize().unwrap();
        let d = Chunk::parse(&b).unwrap();
        assert_eq!(d.pool, c.pool);
        let lists: Vec<u8> = d.groups[0].prims.iter().map(|p| p.list).collect();
        assert_eq!(lists, [3, 4, 7]);
        let mut gt4 = quad(7, [0, 1, 2, 3]);
        gt4.code = 0x3C;
        assert_eq!(d.groups[0].prims[2], gt4);
        // Flat lists keep one colour; the parser repeats it.
        let ft4 = d.groups[0].prims[0];
        assert_eq!((ft4.code, ft4.rgb[3]), (0x2C, [128, 64, 32]));
        assert_eq!(d.groups[0].prims[1].code, 0x32);
        assert_eq!(d.heights, c.heights);
        assert_eq!(d.zones, c.zones);
        assert_eq!(d.serialize().unwrap(), b);

        gt4.idx[3] = 5;
        c.groups[0].prims.push(gt4);
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_BAD_INDEX");
    }

    #[test]
    fn a_new_area_is_walkable_ground_with_walled_edges() {
        let c = Chunk::new_area(1330);
        let d = Chunk::parse(&c.serialize().unwrap()).unwrap();
        assert_eq!((d.groups.len(), d.pool.len()), (64, 81));
        assert_eq!((d.heights[64 * 128 + 64], d.heights[0], d.heights[127 * 128 + 5]), (-129, 0, 0));
        // Slot 2 (1330 & 3): the page where the game puts slot 2, the whole column, 32 palettes.
        assert_eq!((d.texture.image.x, d.texture.image.h), (768, 512));
        assert_eq!(d.texture.clut.as_ref().map(|(r, _)| (r.x, r.y, r.w, r.h)), Some((96, 480, 32, 16)));
        assert!(c.serialize().unwrap().len() == crate::world::CHUNK_BYTES && c.sizes().unwrap().texture <= crate::world::TIM_BUDGET);
        // Floor order: v0 → v1 runs +x, v0 → v2 runs −z (as retail ground).
        let q = d.groups[0].prims[0];
        let p = |k: usize| d.pool[q.idx[k] as usize];
        assert!(p(1)[0] > p(0)[0] && p(2)[2] < p(0)[2]);
    }

    #[test]
    fn rows_share_data_only_with_the_row_before() {
        let mut h = vec![0i16; CELLS];
        h[128..256].fill(-129);
        let (rows, data) = encode_heights(&h);
        // Row 2 repeats row 0 but follows row 1, so it gets its own copy;
        // rows 3.. reuse row 2's.
        assert_eq!(rows[0], 0);
        assert_eq!(rows[1], 2);
        assert_eq!(rows[2], 4);
        assert!(rows[3..].iter().all(|&r| r == 4));
        assert_eq!(data.len(), 12);
    }

    #[test]
    fn a_single_positive_height_is_reported_and_written_as_zero() {
        let mut c = Chunk::blank(0);
        c.heights[3] = 16;
        c.heights[200] = 16;
        c.heights[201] = 16;
        assert_eq!(c.unencodable_heights(), vec![3]);
        let d = Chunk::parse(&c.serialize().unwrap()).unwrap();
        assert_eq!((d.heights[3], d.heights[200], d.heights[201]), (0, 16, 16));
    }

    #[test]
    fn paint_writes_cells_and_checks_them() {
        let base = Chunk::blank(0).serialize().unwrap();
        let p = paint(&base, &base, &[CellWrite { cell: 130, value: -129 }, CellWrite { cell: 131, value: -129 }, CellWrite { cell: 5, value: 16 }], &[CellWrite { cell: 7, value: 68 }]).unwrap();
        let d = Chunk::parse(&p.bytes).unwrap();
        assert_eq!((d.heights[130], d.heights[131], d.heights[5], d.zones[7]), (-129, -129, 0, 68));
        assert_eq!(p.clamped, vec![5]);
        // Painting the old values back gives the old file.
        let back = paint(&base, &p.bytes, &[CellWrite { cell: 130, value: 0 }, CellWrite { cell: 131, value: 0 }], &[CellWrite { cell: 7, value: 0 }]).unwrap();
        assert_eq!(back.bytes, base);
        let bad = |h: &[CellWrite], z: &[CellWrite]| paint(&base, &base, h, z).unwrap_err().code;
        assert_eq!(bad(&[CellWrite { cell: 16384, value: 0 }], &[]), "CHUNK_CELL");
        assert_eq!(bad(&[CellWrite { cell: 0, value: 40000 }], &[]), "CHUNK_CELL_VALUE");
        assert_eq!(bad(&[], &[CellWrite { cell: 0, value: 256 }]), "CHUNK_CELL_VALUE");
    }

    #[test]
    fn budgets_and_shapes_are_enforced() {
        let mut c = Chunk::blank(0);
        // A checkerboard of heights: every cell is its own token.
        for (i, h) in c.heights.iter_mut().enumerate() {
            *h = -((i + i / 128) as i16 % 2);
        }
        assert_eq!(c.sizes().unwrap().layer_a, 128 * 128 * 2);
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_LAYER_A_BUDGET");

        let mut c = Chunk::blank(0);
        c.zones.iter_mut().enumerate().for_each(|(i, z)| *z = ((i + i / 128) % 2) as u8);
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_LAYER_B_BUDGET");

        let mut c = Chunk::blank(0);
        c.pool = vec![[0; 3]; 3];
        let tri = Prim { list: 0, code: 0, n: 3, idx: [0, 1, 2, 0], rgb: [[0; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        c.groups.push(Group { centre: [0; 3], prims: vec![tri; 15_000] });
        assert_eq!(c.sizes().unwrap().mesh, 4 + 16 + 24 + 8 * 4 + 15_000 * 12);
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_MESH_BUDGET");

        let mut c = Chunk::blank(0);
        c.texture.image.h = 512;
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_TIM_SHAPE");
        c.texture.image.w = 70;
        c.texture.pixels = vec![0; 70 * 512 * 2];
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_TIM_BUDGET");

        let mut c = Chunk::blank(0);
        c.zones.pop();
        assert_eq!(c.serialize().unwrap_err().code, "CHUNK_LAYER_SIZE");
        assert_eq!(Chunk::parse(&[0; 16]).unwrap_err().code, "CHUNK_SIZE");
    }
}
