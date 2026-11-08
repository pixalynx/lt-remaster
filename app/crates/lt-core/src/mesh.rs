//! Render meshes for the WebGL viewports (screens 06, 08, 09, 10): TMD models
//! and world chunks flattened to one polygon list in draw order, with their
//! textures decoded from a simulated VRAM into a cropped RGBA atlas, plus the
//! morph targets of a pose block. The binary layouts are read by
//! `src/lib/api/render3d.ts`.
//!
//! ```text
//! mesh  "LT3D" header (64 B)
//!       vertices  n × {s16 x, y, z, pad}           model / chunk-local units, Y down
//!       normals   n × {s16 x, y, z, pad}           unit length 4096
//!       groups    n × {s16 cx, cy, cz, pad; u32 first, count}   TMD objects / chunk draw groups
//!       tiles     n × {u16 tpage, clut, x, y, w, h, u0, v0}     atlas rect of one (tpage, CLUT) crop
//!       polygons  n × 64 B (see [`Poly::write`]), in the engine's insertion order
//!       atlas     w × h × RGBA8   A: 0 clear (texel 0x0000), 255 opaque, 128 STP set, 64 unowned (hatch)
//! poses "LTPS" {u32 count, u32 vertices, u32 0} then per pose {u32 first, u32 count, s16[3] × count, pad to 4}
//! ```
//!
//! Draw order follows the engine: TMD polygons are built in 20 class passes per
//! object (`Model_BuildMeshPackets` @ 0x8018D330), chunk polygons group by group
//! and list by list (`WorldChunk_DrawObjectMesh`). The ordering table key is
//! the nearest vertex for models and the farthest for chunks (header flag 1).

use std::collections::BTreeMap;

use crate::container::{le32, le_i16};
use crate::tim::Tim;
use crate::vram::{c5, Vram, VRAM_H, VRAM_W, WATER_RECT, WATER_RGB};
use crate::world;

pub const MESH_MAGIC: u32 = u32::from_le_bytes(*b"LT3D");
pub const POSE_MAGIC: u32 = u32::from_le_bytes(*b"LTPS");
pub const POLY_BYTES: usize = 64;

/// The 20 TMD classes the engine builds, in build-pass order (research/02 §1.2).
pub const TMD_CLASSES: [u16; 20] = [
    0x2400, 0x2C00, 0x3400, 0x3C00, 0x2501, 0x2D01, 0x3501, 0x3D01, 0x2000, 0x2800, 0x3000, 0x3800, 0x2004, 0x2804, 0x3004,
    0x3804, 0x2101, 0x2901, 0x3101, 0x3901,
];

/// Polygon flags (byte 1 of a polygon record).
pub mod flag {
    pub const TEXTURED: u8 = 1;
    pub const SEMI: u8 = 2;
    /// Texture is not modulated by the vertex colour.
    pub const RAW: u8 = 4;
    /// Lit by the GTE (normals); prelit classes carry baked colours.
    pub const LIT: u8 = 8;
    /// One normal per vertex (else one per polygon).
    pub const SMOOTH: u8 = 16;
    /// One colour per vertex (else one per polygon).
    pub const GOURAUD: u8 = 32;
    /// Samples VRAM that no painted file owns.
    pub const UNOWNED: u8 = 64;
    pub const QUAD: u8 = 128;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Poly {
    /// 3 or 4 (quads are TMD "Z" order: triangles 0-1-2 and 1-3-2).
    pub n: u8,
    pub flags: u8,
    /// Semi-transparency mode 0–3 (tpage bits 5–6; 0 for untextured, unverified).
    pub abr: u8,
    /// TMD build pass 0–19, or chunk list 0–7 (F3 FT3 F4 FT4 G3 GT3 G4 GT4).
    pub slot: u8,
    /// TMD `mode << 8 | flag` (ABE kept); chunk `code << 8 | list`.
    pub class: u16,
    /// File-order index inside its object (TMD) or group (chunk).
    pub prim: u16,
    pub tpage: u16,
    pub clut: u16,
    /// TMD object or chunk draw group.
    pub group: u16,
    /// Atlas tile, 0xFFFF when untextured.
    pub tile: u16,
    pub v: [u16; 4],
    /// Normal per corner, 0xFFFF when prelit.
    pub nrm: [u16; 4],
    /// `r, g, b, code` per corner (128 = 1.0 under a texture).
    pub rgb: [[u8; 4]; 4],
    /// Page texel coordinates per corner (rewritten to atlas coordinates by [`build_atlas`]).
    pub uv: [[u16; 2]; 4],
}

impl Poly {
    pub fn textured(&self) -> bool {
        self.flags & flag::TEXTURED != 0
    }

    /// Writes the 64-byte record.
    ///
    /// ```text
    /// +0 u8 n, flags, abr, slot  +4 u16 class, prim, tpage, clut, group, tile
    /// +16 u16 v[4]  +24 u16 nrm[4]  +32 u8 rgbc[4][4]  +48 u16 uv[4][2]
    /// ```
    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.n, self.flags, self.abr, self.slot]);
        for h in [self.class, self.prim, self.tpage, self.clut, self.group, self.tile] {
            out.extend_from_slice(&h.to_le_bytes());
        }
        for h in self.v.iter().chain(self.nrm.iter()) {
            out.extend_from_slice(&h.to_le_bytes());
        }
        for c in &self.rgb {
            out.extend_from_slice(c);
        }
        for uv in &self.uv {
            out.extend_from_slice(&uv[0].to_le_bytes());
            out.extend_from_slice(&uv[1].to_le_bytes());
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Group {
    pub centre: [i16; 3],
    pub first: u32,
    pub count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct RenderMesh {
    /// false = TMD model, true = world chunk.
    pub chunk: bool,
    pub verts: Vec<[i16; 3]>,
    pub normals: Vec<[i16; 3]>,
    pub groups: Vec<Group>,
    /// In the engine's insertion order.
    pub polys: Vec<Poly>,
    /// Primitives of classes the engine skips (`Model_BuildMeshPackets` has no default branch).
    pub skipped: u32,
}

impl RenderMesh {
    /// Min x, y, z and max x, y, z over the vertices the polygons use.
    pub fn bbox(&self) -> [i16; 6] {
        let mut b = [i16::MAX, i16::MAX, i16::MAX, i16::MIN, i16::MIN, i16::MIN];
        for p in &self.polys {
            for &i in &p.v[..p.n as usize] {
                if let Some(v) = self.verts.get(i as usize) {
                    for a in 0..3 {
                        b[a] = b[a].min(v[a]);
                        b[a + 3] = b[a + 3].max(v[a]);
                    }
                }
            }
        }
        if b[0] > b[3] {
            [0; 6]
        } else {
            b
        }
    }

    /// The longest side of [`Self::bbox`].
    pub fn extent(&self) -> i32 {
        let b = self.bbox().map(i32::from);
        (b[3] - b[0]).max(b[4] - b[1]).max(b[5] - b[2])
    }
}

/* ───────────────────────── TMD ───────────────────────── */

/// Record layout of a TMD primitive, from its `mode`/`flag` bytes
/// (research/02 §1.2): uv words, then colour words, then index words.
struct Layout {
    n: usize,
    textured: bool,
    lit: bool,
    /// Lit: one normal per vertex. Prelit: one colour per vertex.
    smooth: bool,
    colours: usize,
}

fn layout(mode: u8, flag: u8) -> Layout {
    let n = if mode & 0x08 != 0 { 4 } else { 3 };
    let textured = mode & 0x04 != 0;
    let iip = mode & 0x10 != 0;
    let lit = flag & 0x01 == 0;
    let grd = flag & 0x04 != 0;
    let colours = match (lit, textured) {
        (true, true) => 0,
        (true, false) => {
            if grd {
                n
            } else {
                1
            }
        }
        (false, _) => {
            if iip {
                n
            } else {
                1
            }
        }
    };
    Layout { n, textured, lit, smooth: iip, colours }
}

/// Every object of a TMD as one mesh: vertices and normals concatenated, and
/// polygons in build order (object, then the 20 class passes, then file
/// order). `None` if `b` is not a TMD.
pub fn tmd_mesh(b: &[u8]) -> Option<RenderMesh> {
    if b.len() < 12 || le32(b, 0) != 0x41 {
        return None;
    }
    let fixp = le32(b, 4) & 1 != 0;
    let nobj = le32(b, 8) as usize;
    if nobj > 256 || 12 + nobj * 28 > b.len() {
        return None;
    }
    let table = 12usize;
    let rel = |o: u32| if fixp { o as usize } else { table + o as usize };
    let svec = |at: usize| -> [i16; 3] {
        if at + 6 <= b.len() {
            [le_i16(b, at), le_i16(b, at + 2), le_i16(b, at + 4)]
        } else {
            // toy/34 Minicar declares one normal more than the file holds.
            [0, 0, 0]
        }
    };
    let mut m = RenderMesh::default();
    for o in 0..nobj {
        let at = table + o * 28;
        let field = |k: usize| le32(b, at + k * 4);
        let (vert_top, n_vert, norm_top, n_norm, prim_top, n_prim) = (field(0), field(1), field(2), field(3), field(4), field(5));
        if n_vert > 0x10000 || n_norm > 0x10000 || n_prim > 0x10000 {
            return None;
        }
        let v0 = m.verts.len();
        let n0 = m.normals.len();
        m.verts.extend((0..n_vert as usize).map(|i| svec(rel(vert_top) + i * 8)));
        m.normals.extend((0..n_norm as usize).map(|i| svec(rel(norm_top) + i * 8)));
        let first = m.polys.len();
        let mut parsed: Vec<Poly> = Vec::with_capacity(n_prim as usize);
        let mut p = rel(prim_top);
        for index in 0..n_prim as usize {
            let Some(hdr) = b.get(p..p + 4) else { break };
            let (ilen, flag_b, mode) = (hdr[1] as usize, hdr[2], hdr[3]);
            let words = b.get(p + 4..p + 4 + ilen * 4);
            p += 4 + ilen * 4;
            let key = ((mode as u16) << 8 | flag_b as u16) & !0x200;
            let Some(slot) = TMD_CLASSES.iter().position(|&k| k == key) else {
                m.skipped += 1;
                continue;
            };
            let Some(words) = words else {
                m.skipped += 1;
                continue;
            };
            match tmd_poly(words, mode, flag_b, slot as u8, index as u16, o as u16, v0, n0) {
                Some(poly) => parsed.push(poly),
                None => m.skipped += 1,
            }
        }
        // Build passes: class slot, then file order (stable sort keeps file order).
        parsed.sort_by_key(|p| p.slot);
        m.polys.extend(parsed);
        m.groups.push(Group { centre: [0; 3], first: first as u32, count: (m.polys.len() - first) as u32 });
    }
    // Indices past the vertex / normal pools would read garbage in the engine; drop them here.
    let (nv, nn) = (m.verts.len(), m.normals.len());
    let before = m.polys.len();
    m.polys.retain(|p| {
        p.v[..p.n as usize].iter().all(|&i| (i as usize) < nv)
            && (p.flags & flag::LIT == 0 || p.nrm[..p.n as usize].iter().all(|&i| (i as usize) < nn))
    });
    m.skipped += (before - m.polys.len()) as u32;
    // Re-derive group ranges after the filter.
    let mut start = 0u32;
    for (g, grp) in m.groups.iter_mut().enumerate() {
        let count = m.polys.iter().filter(|p| p.group as usize == g).count() as u32;
        *grp = Group { centre: [0; 3], first: start, count };
        start += count;
    }
    Some(m)
}

/// Moves the vertices that TMD object `object`'s polygons use by `dy` (a
/// two-part field object's lid sits 0x28 above its base: `Npc_DrawIfVisible`).
pub fn lift_object(m: &mut RenderMesh, object: u16, dy: i16) {
    let mut moved = vec![false; m.verts.len()];
    for p in m.polys.iter().filter(|p| p.group == object) {
        for &i in &p.v[..p.n as usize] {
            let i = i as usize;
            if !moved[i] {
                moved[i] = true;
                m.verts[i][1] = m.verts[i][1].saturating_add(dy);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tmd_poly(w: &[u8], mode: u8, flag_b: u8, slot: u8, prim: u16, group: u16, v0: usize, n0: usize) -> Option<Poly> {
    let l = layout(mode, flag_b);
    let word = |i: usize| -> Option<u32> { w.get(i * 4..i * 4 + 4).map(|s| le32(s, 0)) };
    let lo = |v: u32| (v & 0xFFFF) as u16;
    let hi = |v: u32| (v >> 16) as u16;
    let n = l.n;
    let semi = mode & 0x02 != 0;
    let mut poly = Poly { n: n as u8, slot, class: (mode as u16) << 8 | flag_b as u16, prim, group, tile: 0xFFFF, ..Default::default() };
    let mut k = 0usize;
    if l.textured {
        for c in 0..n {
            let uvw = word(k + c)?;
            poly.uv[c] = [(uvw & 0xFF) as u16, ((uvw >> 8) & 0xFF) as u16];
        }
        poly.clut = hi(word(k)?);
        poly.tpage = hi(word(k + 1)?);
        k += n;
    }
    // Lit textured classes have no colour word: the GTE writes the light colour (128 = 1.0 here).
    let mut cols = [[128u8, 128, 128, 0]; 4];
    for (c, col) in cols.iter_mut().enumerate().take(l.colours) {
        let cw = word(k + c)?;
        *col = [cw as u8, (cw >> 8) as u8, (cw >> 16) as u8, (cw >> 24) as u8];
    }
    if l.colours == 1 {
        cols = [cols[0]; 4];
    } else if n == 3 {
        cols[3] = cols[2];
    }
    if l.colours == 0 {
        cols = [[128, 128, 128, mode]; 4];
    }
    poly.rgb = cols;
    k += l.colours;
    let (mut v, mut nrm) = ([0u16; 4], [0xFFFFu16; 4]);
    if l.lit {
        if l.smooth {
            for c in 0..n {
                let iw = word(k + c)?;
                nrm[c] = lo(iw);
                v[c] = hi(iw);
            }
        } else {
            let a = word(k)?;
            let bw = word(k + 1)?;
            nrm = [lo(a); 4];
            v[0] = hi(a);
            v[1] = lo(bw);
            v[2] = hi(bw);
            if n == 4 {
                v[3] = lo(word(k + 2)?);
            }
        }
    } else {
        let a = word(k)?;
        let bw = word(k + 1)?;
        v = [lo(a), hi(a), lo(bw), if n == 4 { hi(bw) } else { 0 }];
    }
    for c in 0..n {
        v[c] = u16::try_from(v[c] as usize + v0).ok()?;
        if l.lit {
            nrm[c] = u16::try_from(nrm[c] as usize + n0).ok()?;
        }
    }
    if n == 3 {
        v[3] = v[2];
        nrm[3] = nrm[2];
        poly.uv[3] = poly.uv[2];
    }
    poly.v = v;
    poly.nrm = nrm;
    let mut f = 0u8;
    if l.textured {
        f |= flag::TEXTURED;
    }
    if semi {
        f |= flag::SEMI;
    }
    if l.lit {
        f |= flag::LIT;
        if l.smooth {
            f |= flag::SMOOTH;
        }
    }
    if l.colours > 1 {
        f |= flag::GOURAUD;
    }
    if n == 4 {
        f |= flag::QUAD;
    }
    // Prelit textured classes are built with `gpuCode = mode & 0xFE`: always modulated.
    poly.flags = f;
    poly.abr = if l.textured { ((poly.tpage >> 5) & 3) as u8 } else { 0 };
    Some(poly)
}

/* ───────────────────────── Chunks ───────────────────────── */

/// A parsed world chunk (`world::mesh`) as a render mesh: baked vertex
/// colours, no lighting, group order then list order then record order.
pub fn chunk_mesh(m: &world::Mesh) -> RenderMesh {
    let mut out = RenderMesh { chunk: true, verts: m.pool.clone(), ..Default::default() };
    for g in &m.groups {
        let first = out.polys.len() as u32;
        for (k, p) in m.prims[g.first as usize..(g.first + g.count) as usize].iter().enumerate() {
            let n = p.n as usize;
            let mut poly = Poly {
                n: p.n,
                slot: p.list,
                class: (p.code as u16) << 8 | p.list as u16,
                prim: k as u16,
                group: g.index as u16,
                tile: 0xFFFF,
                tpage: if p.textured() { p.tpage } else { 0 },
                clut: if p.textured() { p.clut } else { 0 },
                v: p.idx,
                nrm: [0xFFFF; 4],
                ..Default::default()
            };
            for c in 0..4 {
                let s = if c < n { c } else { n - 1 };
                poly.rgb[c] = [p.rgb[s][0], p.rgb[s][1], p.rgb[s][2], p.code];
                poly.uv[c] = [p.uv[s][0] as u16, p.uv[s][1] as u16];
            }
            if n == 3 {
                poly.v[3] = poly.v[2];
            }
            let mut f = 0u8;
            if p.textured() {
                f |= flag::TEXTURED;
                if p.raw() {
                    f |= flag::RAW;
                }
            }
            if p.semi() {
                f |= flag::SEMI;
            }
            if p.gouraud() {
                f |= flag::GOURAUD;
            }
            if n == 4 {
                f |= flag::QUAD;
            }
            poly.flags = f;
            poly.abr = if p.textured() { ((p.tpage >> 5) & 3) as u8 } else { 0 };
            if poly.v[..n].iter().all(|&i| (i as usize) < out.verts.len()) {
                out.polys.push(poly);
            } else {
                out.skipped += 1;
            }
        }
        out.groups.push(Group { centre: g.centre, first, count: out.polys.len() as u32 - first });
    }
    out
}

/* ───────────────────────── VRAM with ownership ───────────────────────── */

/// A VRAM model that remembers which halfwords a painted file owns, so
/// texels nobody uploads can be shown as such (PRD 06 §4).
#[derive(Clone)]
pub struct OwnedVram {
    pub vram: Vram,
    owned: Vec<u64>,
    /// Treat 4bpp texels left at 0 in the field water rect as runtime water.
    pub water: bool,
}

impl Default for OwnedVram {
    fn default() -> Self {
        Self::new(false)
    }
}

impl OwnedVram {
    pub fn new(water: bool) -> Self {
        Self { vram: Vram::new(), owned: vec![0; VRAM_W * VRAM_H / 64], water }
    }

    /// A whole video memory read from a running machine (1024 × 512 halfwords): every texel
    /// owned, since the game put it there.
    pub fn whole(mem: Vec<u16>) -> Self {
        assert_eq!(mem.len(), VRAM_W * VRAM_H, "a whole VRAM");
        Self { vram: Vram { mem }, owned: vec![u64::MAX; VRAM_W * VRAM_H / 64], water: false }
    }

    fn own_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        for yy in y..y + h {
            for xx in x..x + w {
                let i = (yy & (VRAM_H - 1)) * VRAM_W + (xx & (VRAM_W - 1));
                self.owned[i / 64] |= 1 << (i % 64);
            }
        }
    }

    #[inline]
    pub fn owns(&self, x: usize, y: usize) -> bool {
        let i = (y & (VRAM_H - 1)) * VRAM_W + (x & (VRAM_W - 1));
        self.owned[i / 64] & (1 << (i % 64)) != 0
    }

    /// Uploads a TIM to the rectangles its header names. False if `b` is not one.
    pub fn paint_tim(&mut self, b: &[u8]) -> bool {
        let Some(t) = Tim::parse(b) else { return false };
        if !self.vram.paint_tim(b) {
            return false;
        }
        let r = t.info.image;
        self.own_rect(r.x as usize, r.y as usize, r.w as usize, r.h as usize);
        if let Some(c) = t.info.clut {
            self.own_rect(c.x as usize, c.y as usize, c.w as usize, c.h as usize);
        }
        true
    }

    /// `BattleUI_CopyVramRect`: VRAM → VRAM (owned if the source is).
    pub fn copy_rect(&mut self, sx: usize, sy: usize, w: usize, h: usize, dx: usize, dy: usize) {
        for yy in 0..h {
            for xx in 0..w {
                let v = self.vram.get(sx + xx, sy + yy);
                let (tx, ty) = ((dx + xx) & (VRAM_W - 1), (dy + yy) & (VRAM_H - 1));
                self.vram.mem[ty * VRAM_W + tx] = v;
                if self.owns(sx + xx, sy + yy) {
                    self.own_rect(tx, ty, 1, 1);
                }
            }
        }
    }

    /// RGBA of texel (u, v) of `tpage` through `clut` (atlas alpha encoding).
    pub fn texel_rgba(&self, tpage: u16, clut: u16, u: u8, v: u8) -> [u8; 4] {
        let px = (tpage as usize & 0xF) * 64;
        let py = ((tpage as usize >> 4) & 1) * 256;
        let (cx, cy) = ((clut as usize & 0x3F) * 16, (clut as usize >> 6) & 0x1FF);
        let (u, v) = (u as usize, v as usize);
        let depth = (tpage >> 7) & 3;
        let (hx, hy) = match depth {
            0 => (px + u / 4, py + v),
            1 => (px + u / 2, py + v),
            _ => (px + u, py + v),
        };
        let hatch = || {
            if (u + v) / 2 % 2 == 0 {
                [255, 0, 255, 64]
            } else {
                [72, 0, 72, 64]
            }
        };
        if !self.owns(hx, hy) {
            return hatch();
        }
        let hw = self.vram.get(hx, hy);
        let colour = match depth {
            0 | 1 => {
                let index = if depth == 0 { (hw >> ((u & 3) * 4)) & 0xF } else { (hw >> ((u & 1) * 8)) & 0xFF } as usize;
                if self.water && hw == 0 && in_water(hx, hy) {
                    return [WATER_RGB[0], WATER_RGB[1], WATER_RGB[2], 255];
                }
                if !self.owns(cx + index, cy) {
                    return hatch();
                }
                self.vram.get(cx + index, cy)
            }
            _ => hw,
        };
        match colour {
            0 => [0, 0, 0, 0],
            c => [c5(c), c5(c >> 5), c5(c >> 10), if c & 0x8000 != 0 { 128 } else { 255 }],
        }
    }

    fn texel_owned(&self, tpage: u16, clut: u16, u: u8, v: u8) -> bool {
        self.texel_rgba(tpage, clut, u, v)[3] != 64
    }
}

#[inline]
fn in_water(x: usize, y: usize) -> bool {
    let (wx, wy, ww, wh) = WATER_RECT;
    (wx..wx + ww).contains(&x) && (wy..wy + wh).contains(&y)
}

/// Battle context for Living Toys (PRD 06 §4): SYS14 `sys/2` → SYS06 `sys/4`
/// (TIM in section 0) → SYS17 `sys/19` → the animated effect palette at
/// (144, 480), shown with rows (304, 496..504) of SYS06 (a representative
/// source, unverified). The toy's own TIM goes on top (slot 0).
pub fn battle_vram(sys2: Option<&[u8]>, sys4: Option<&[u8]>, sys19: Option<&[u8]>) -> OwnedVram {
    let mut v = OwnedVram::new(false);
    if let Some(b) = sys2 {
        v.paint_tim(b);
    }
    if let Some(b) = sys4 {
        if b.len() >= 16 {
            let ofs = le32(b, 0) as usize;
            if let Some(t) = b.get(ofs..) {
                v.paint_tim(t);
            }
        }
    }
    if let Some(b) = sys19 {
        v.paint_tim(b);
    }
    v.copy_rect(304, 496, 16, 9, 144, 480);
    v
}

/* ───────────────────────── Atlas ───────────────────────── */

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tile {
    pub tpage: u16,
    pub clut: u16,
    /// Atlas rect.
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Page texel at the rect's top-left.
    pub u0: u16,
    pub v0: u16,
}

pub struct Atlas {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub tiles: Vec<Tile>,
}

/// Decodes each used (tpage, CLUT) pair once, cropped to the texels its
/// polygons can reach, packs the crops into one RGBA atlas and rewrites the
/// polygons' UVs to atlas texels. Polygons whose crop holds unowned texels
/// get [`flag::UNOWNED`].
pub fn build_atlas(mesh: &mut RenderMesh, vram: &OwnedVram) -> Atlas {
    // Lookup key: page bits + depth (ABR does not change the texels); CLUT ignored at 15bpp.
    let key = |p: &Poly| {
        let page = p.tpage & 0x19F;
        let clut = if (page >> 7) & 3 >= 2 { 0 } else { p.clut };
        (page, clut)
    };
    let mut boxes: BTreeMap<(u16, u16), [u16; 4]> = BTreeMap::new();
    for p in mesh.polys.iter().filter(|p| p.textured()) {
        let b = boxes.entry(key(p)).or_insert([255, 255, 0, 0]);
        for c in 0..p.n as usize {
            let [u, v] = p.uv[c];
            b[0] = b[0].min(u);
            b[1] = b[1].min(v);
            b[2] = b[2].max(u);
            b[3] = b[3].max(v);
        }
    }
    let mut tiles: Vec<Tile> = boxes
        .iter()
        .map(|(&(tpage, clut), b)| Tile { tpage, clut, x: 0, y: 0, w: b[2] - b[0] + 1, h: b[3] - b[1] + 1, u0: b[0], v0: b[1] })
        .collect();
    // Shelf packing, tallest first.
    let area: u32 = tiles.iter().map(|t| t.w as u32 * t.h as u32).sum();
    let widest = tiles.iter().map(|t| t.w as u32).max().unwrap_or(1);
    let width = (((area as f64).sqrt() * 1.15).ceil() as u32).max(widest).max(16).div_ceil(16) * 16;
    let mut order: Vec<usize> = (0..tiles.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((tiles[i].h, tiles[i].w)));
    let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
    for &i in &order {
        let t = &mut tiles[i];
        if x + t.w as u32 > width {
            x = 0;
            y += shelf;
            shelf = 0;
        }
        t.x = x as u16;
        t.y = y as u16;
        x += t.w as u32;
        shelf = shelf.max(t.h as u32);
    }
    let height = (y + shelf).max(1);
    let width = width.max(1);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let mut unowned = vec![false; tiles.len()];
    for (ti, t) in tiles.iter().enumerate() {
        for ty in 0..t.h as usize {
            for tx in 0..t.w as usize {
                let c = vram.texel_rgba(t.tpage, t.clut, (t.u0 as usize + tx) as u8, (t.v0 as usize + ty) as u8);
                if c[3] == 64 {
                    unowned[ti] = true;
                }
                let o = (((t.y as usize + ty) * width as usize) + t.x as usize + tx) * 4;
                rgba[o..o + 4].copy_from_slice(&c);
            }
        }
    }
    let index: BTreeMap<(u16, u16), usize> = tiles.iter().enumerate().map(|(i, t)| ((t.tpage, t.clut), i)).collect();
    for p in mesh.polys.iter_mut().filter(|p| p.textured()) {
        let ti = index[&key(p)];
        let t = tiles[ti];
        p.tile = ti as u16;
        if unowned[ti] {
            // Only flag polygons whose own UV box reaches an unowned texel.
            let (mut u0, mut v0, mut u1, mut v1) = (255u16, 255u16, 0u16, 0u16);
            for c in 0..p.n as usize {
                u0 = u0.min(p.uv[c][0]);
                v0 = v0.min(p.uv[c][1]);
                u1 = u1.max(p.uv[c][0]);
                v1 = v1.max(p.uv[c][1]);
            }
            let hit = (v0..=v1).any(|v| (u0..=u1).any(|u| !vram.texel_owned(t.tpage, t.clut, u as u8, v as u8)));
            if hit {
                p.flags |= flag::UNOWNED;
            }
        }
        for c in 0..4 {
            p.uv[c] = [p.uv[c][0] - t.u0 + t.x, p.uv[c][1] - t.v0 + t.y];
        }
    }
    Atlas { w: width, h: height, rgba, tiles }
}

/* ───────────────────────── Encoding ───────────────────────── */

/// The "LT3D" payload (layout in the module docs).
pub fn encode(mesh: &RenderMesh, atlas: &Atlas) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        64 + mesh.verts.len() * 8 + mesh.normals.len() * 8 + mesh.groups.len() * 16 + atlas.tiles.len() * 16 + mesh.polys.len() * POLY_BYTES + atlas.rgba.len(),
    );
    let u32s = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    u32s(&mut out, MESH_MAGIC);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(mesh.chunk as u16).to_le_bytes());
    for n in [mesh.verts.len(), mesh.normals.len(), mesh.polys.len(), atlas.tiles.len(), mesh.groups.len()] {
        u32s(&mut out, n as u32);
    }
    u32s(&mut out, atlas.w);
    u32s(&mut out, atlas.h);
    for b in mesh.bbox() {
        out.extend_from_slice(&b.to_le_bytes());
    }
    u32s(&mut out, mesh.skipped);
    // Flag 1: the ordering-table key is the farthest vertex (chunks), else the nearest.
    u32s(&mut out, if mesh.chunk { 1 } else { 0 });
    u32s(&mut out, 0);
    u32s(&mut out, 0);
    debug_assert_eq!(out.len(), 64);
    for v in mesh.verts.iter().chain(mesh.normals.iter()) {
        for c in v {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&0i16.to_le_bytes());
    }
    for g in &mesh.groups {
        for c in g.centre {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&0i16.to_le_bytes());
        u32s(&mut out, g.first);
        u32s(&mut out, g.count);
    }
    for t in &atlas.tiles {
        for h in [t.tpage, t.clut, t.x, t.y, t.w, t.h, t.u0, t.v0] {
            out.extend_from_slice(&h.to_le_bytes());
        }
    }
    for p in &mesh.polys {
        p.write(&mut out);
    }
    out.extend_from_slice(&atlas.rgba);
    out
}

/* ───────────────────────── Morph targets ───────────────────────── */

/// One pose's first record (the engine applies only the first, `FindPoseRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MorphTarget {
    pub first: u32,
    /// Deltas for vertices `first..first + deltas.len()`, model units.
    pub deltas: Vec<[i16; 3]>,
}

impl MorphTarget {
    /// Largest absolute delta on any axis.
    pub fn max_delta(&self) -> u16 {
        self.deltas.iter().flat_map(|d| d.iter().map(|c| c.unsigned_abs())).max().unwrap_or(0)
    }
}

/// The poses of a pose block ("Group B", `pose.rs`), 1-based in the game:
/// `targets[k]` is pose k + 1. `None` if the block does not parse.
pub fn morph_targets(b: &[u8]) -> Option<Vec<MorphTarget>> {
    let n = le32(b.get(0..4)?, 0);
    if n > 256 {
        return None;
    }
    let mut p = 4usize;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let mut first: Option<MorphTarget> = None;
        loop {
            let hdr = b.get(p..p + 12)?;
            let (more, start, count) = (le32(hdr, 0), le32(hdr, 4), le32(hdr, 8) as usize);
            let data = b.get(p + 12..p + 12 + count * 6)?;
            if first.is_none() {
                let deltas = (0..count).map(|k| [le_i16(data, k * 6), le_i16(data, k * 6 + 2), le_i16(data, k * 6 + 4)]).collect();
                first = Some(MorphTarget { first: start, deltas });
            }
            p += 12 + count * 6;
            if more == 0 {
                break;
            }
        }
        out.push(first?);
    }
    Some(out)
}

/// The "LTPS" payload for `vertices` base vertices.
pub fn encode_poses(targets: &[MorphTarget], vertices: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [POSE_MAGIC, targets.len() as u32, vertices, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for t in targets {
        out.extend_from_slice(&t.first.to_le_bytes());
        out.extend_from_slice(&(t.deltas.len() as u32).to_le_bytes());
        for d in &t.deltas {
            for c in d {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

/// Live vertices for a blend, exactly as `BlendTwoPoseAnimation` @ 0x8019593C
/// computes them: base + Σ (delta · (w << 4)) >> 12 over (a, wa) and (b, wb),
/// arithmetic shift, wrapping s16. Poses are 1-based; 0 = base.
pub fn blend(base: &[[i16; 3]], targets: &[MorphTarget], a: u8, wa: u16, b: u8, wb: u16) -> Vec<[i16; 3]> {
    let mut out = base.to_vec();
    for (pose, w) in [(a, wa), (b, wb)] {
        if pose == 0 || w == 0 {
            continue;
        }
        let Some(t) = targets.get(pose as usize - 1) else { continue };
        let ir0 = (w as i32) << 4;
        for (k, d) in t.deltas.iter().enumerate() {
            let Some(v) = out.get_mut(t.first as usize + k) else { break };
            for c in 0..3 {
                v[c] = v[c].wrapping_add(((d[c] as i32 * ir0) >> 12) as i16);
            }
        }
    }
    out
}

/// A model no longer than this each way is folded to a point (see [`preview_pose`]).
pub const FOLDED: i32 = 8;

/// Poses a model for a preview (the Place actor bar's thumbnails, LT-78): pose `first` of its set,
/// as the field first draws it, unless that folds the model to a point (npc/6 actor 10, a ghost a
/// scene grows out of nothing); then the pose of the set where it is biggest. `m.verts` holds the
/// base pose on entry.
pub fn preview_pose(m: &mut RenderMesh, targets: &[MorphTarget], first: u8) {
    let base = std::mem::take(&mut m.verts);
    m.verts = blend(&base, targets, first, 256, 0, 0);
    if m.extent() >= FOLDED {
        return;
    }
    let mut best = (m.extent(), std::mem::take(&mut m.verts));
    for pose in 0..=targets.len().min(255) as u8 {
        m.verts = blend(&base, targets, pose, 256, 0, 0);
        if m.extent() > best.0 {
            best = (m.extent(), std::mem::take(&mut m.verts));
        }
    }
    m.verts = best.1;
}

/* ───────────────────────── Model containers ───────────────────────── */

/// Container layouts that hold a TMD (PRD 06 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// `toy/n`: 8 sections; 7 main, 4 effect, 6 TIM, 0 poses.
    Toy,
    /// `enemy/n` (n ≥ 2): 7 sections; 5 main, 1 effect, 3 TIM, 0 poses.
    Enemy,
    /// `knight/0` and KNIGHT02/03 (`enemy/0`, `enemy/1`): `{poses, model, TIM, profile}`.
    Knight,
    /// `pb/n`: `{poses, subModel, TSQ, TVB, model, profile}`; textures in SYS06.
    Pb,
    /// `bf/n`: `u32 timOfs (8), u32 modelOfs`.
    Bf,
    /// `sys/6` (SYS13): `{knightModel, babyModel, knightPoses, babyPoses}`, field context.
    Sys6Knight,
    Sys6Baby,
}

/// Which VRAM a container's models are drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// SYS14 + SYS06 + SYS17 + effect palette, then the file's own TIM.
    Battle,
    /// SYS14 + SYS08 (field Knight and Baby).
    Field,
}

pub struct ModelParts<'a> {
    pub main: &'a [u8],
    pub effect: Option<&'a [u8]>,
    pub tim: Option<&'a [u8]>,
    pub poses: Option<&'a [u8]>,
    pub context: Context,
}

/// Splits a container into its model, effect model, TIM and pose block.
pub fn model_parts(kind: Container, b: &[u8]) -> Option<ModelParts<'_>> {
    let sec = |n: usize, i: usize| -> Option<&[u8]> {
        let s = crate::container::sections(b, n)?;
        let s = s.get(i)?;
        b.get(s.offset as usize..(s.offset + s.len) as usize)
    };
    Some(match kind {
        Container::Toy => ModelParts { main: sec(8, 7)?, effect: sec(8, 4), tim: sec(8, 6), poses: sec(8, 0), context: Context::Battle },
        Container::Enemy => ModelParts { main: sec(7, 5)?, effect: sec(7, 1), tim: sec(7, 3), poses: sec(7, 0), context: Context::Battle },
        Container::Knight => ModelParts { main: sec(4, 1)?, effect: None, tim: sec(4, 2), poses: sec(4, 0), context: Context::Battle },
        Container::Pb => ModelParts { main: sec(6, 4)?, effect: sec(6, 1), tim: None, poses: sec(6, 0), context: Context::Battle },
        Container::Bf => {
            let (tim, model) = (le32(b.get(0..8)?, 0) as usize, le32(b, 4) as usize);
            ModelParts { main: b.get(model..)?, effect: None, tim: b.get(tim..model), poses: None, context: Context::Battle }
        }
        Container::Sys6Knight => ModelParts { main: sec(4, 0)?, effect: None, tim: None, poses: sec(4, 2), context: Context::Field },
        Container::Sys6Baby => ModelParts { main: sec(4, 1)?, effect: None, tim: None, poses: sec(4, 3), context: Context::Field },
    })
}

/// Group A model `u` of an NPC pack as the model browser shows it: in the first pose
/// of the set its first animated actor gives it ([`preview_pose`]), a two-part
/// object's lid 40 units up. A model no actor animates keeps its base pose.
pub fn npc_model(pack: &crate::npc::NpcPack, b: &[u8], u: &crate::npc::ModelUse) -> Option<RenderMesh> {
    let mut m = tmd_mesh(pack.model(b, u.index as usize)?)?;
    if let (Some(set), Some(pose)) = (u.set, u.pose) {
        if let Some(t) = pack.pose_block(b, set as usize).and_then(morph_targets) {
            preview_pose(&mut m, &t, pose);
        }
    }
    if u.lid {
        lift_object(&mut m, 1, -0x28);
    }
    Some(m)
}

/// Field context for the SYS13 Knight and Baby: SYS14 (the Knight's face) and SYS08.
pub fn field_model_vram(sys2: Option<&[u8]>, sys7: Option<&[u8]>) -> OwnedVram {
    let mut v = OwnedVram::new(false);
    for b in [sys2, sys7].into_iter().flatten() {
        v.paint_tim(b);
    }
    v
}

/* ───────────────────────── Field VRAM for chunks ───────────────────────── */

/// The field VRAM for a chunk with ownership (same painting order as
/// `worldmap::WorldTextures::vram_for`): SYS08 atlas, the `fe/k` region set,
/// sibling interior pages, the chunk's own TIM.
pub fn field_vram(atlas: Option<&[u8]>, set: Option<&[u8]>, siblings: &[Vec<u8>], chunk: &[u8]) -> OwnedVram {
    let mut v = OwnedVram::new(true);
    if let Some(a) = atlas {
        v.paint_tim(a);
    }
    if let Some(s) = set {
        v.paint_tim(s);
        // The animated water frame no file holds (vram.rs): shown as water, not unowned.
        let (wx, wy, ww, wh) = WATER_RECT;
        v.own_rect(wx, wy, ww, wh);
    }
    for s in siblings {
        if let Some(t) = s.get(world::TIM_OFFSET..) {
            v.paint_tim(t);
        }
    }
    if let Some(t) = chunk.get(world::TIM_OFFSET..) {
        v.paint_tim(t);
    }
    v
}
