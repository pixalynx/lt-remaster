//! Prefabs: the construction kit's building blocks (screen 10). A prefab is
//! a piece of a retail chunk (a house, a fence, a wardrobe) cut out by an
//! area of cells: its polygons, the texture pixels and palettes those use,
//! and the walk heights Tamsoft authored under it (which keep collision the
//! geometry can't give, such as unmodelled wardrobe tops).
//!
//! [`harvest`] cuts one out; [`place`] puts it into a chunk: moved and turned
//! in 90° steps, its texture tiles packed into the chunk's own page (palettes
//! reused when identical, else written to a free CLUT slot), its polygons in
//! new draw groups, and its height patch written relative to the ground.
//! Polygons that sample a shared page (the field atlas or a region set)
//! keep their tpage; they look right only where that page is loaded.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::vram::Vram;
use crate::world::Prim;

use super::bake;
use super::chunk::{Chunk, Group, Texture, CELLS, MAX_VERTICES};
use super::{EditError, Result};

/// A rectangle of cells, inclusive: columns `c0..=c1`, rows `r0..=r1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellRect {
    pub c0: u8,
    pub r0: u8,
    pub c1: u8,
    pub r1: u8,
}

impl CellRect {
    pub fn w(&self) -> usize {
        (self.c1 - self.c0) as usize + 1
    }
    pub fn h(&self) -> usize {
        (self.r1 - self.r0) as usize + 1
    }
    /// Chunk-local x, z of the rectangle's min corner.
    fn origin(&self) -> (i32, i32) {
        (self.c0 as i32 * 32 - 2048, self.r0 as i32 * 32 - 2048)
    }
    fn valid(&self) -> bool {
        self.c0 <= self.c1 && self.r0 <= self.r1 && self.c1 < 128 && self.r1 < 128
    }
}

/// A texture tile: pixels (colour indices 0–15, row-major) and the palette.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tile {
    pub w: u16,
    pub h: u16,
    /// One hex digit per pixel.
    pub pixels: String,
    pub palette: Vec<u16>,
    /// Where the texels and the palette sat in VRAM when it was cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TileOrigin>,
}

/// Where a tile's texels and palette sat in VRAM in its source chunk. A
/// chunk that has the same texels and palette there (the same region set,
/// or its own page for the village houses) samples them in place instead
/// of taking a copy ([`PackCache::vram`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileOrigin {
    /// The tpage word's page bits (x / 64, band << 4); 4bpp.
    pub page: u16,
    /// Top-left texel within the 256 × 256 page.
    pub x: u16,
    pub y: u16,
    pub clut: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabPrim {
    pub prim: PrimData,
    /// Own-page tile the UVs are relative to; `None` = untextured or a shared page.
    pub tile: Option<u16>,
}

/// `world::Prim` in a serializable shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimData {
    pub list: u8,
    pub code: u8,
    pub n: u8,
    pub idx: [u16; 4],
    pub rgb: [[u8; 3]; 4],
    pub uv: [[u8; 2]; 4],
    pub clut: u16,
    pub tpage: u16,
}

impl From<Prim> for PrimData {
    fn from(p: Prim) -> Self {
        PrimData { list: p.list, code: p.code, n: p.n, idx: p.idx, rgb: p.rgb, uv: p.uv, clut: p.clut, tpage: p.tpage }
    }
}

impl From<PrimData> for Prim {
    fn from(p: PrimData) -> Self {
        Prim { list: p.list, code: p.code, n: p.n, idx: p.idx, rgb: p.rgb, uv: p.uv, clut: p.clut, tpage: p.tpage }
    }
}

/// A walk-height cell relative to the ground (`h − (ground − 1)`), or blocked (−1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchCell {
    Rel(i16),
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prefab {
    pub name: String,
    pub source_chunk: u32,
    pub area: CellRect,
    /// Ground surface y under the area's border in the source (PSX Y).
    pub ground_y: i16,
    /// Vertices relative to the area's min corner and the ground: x, y, z.
    pub pool: Vec<[i16; 3]>,
    pub prims: Vec<PrefabPrim>,
    pub tiles: Vec<Tile>,
    /// `area.w() × area.h()` cells, row-major (rows = z).
    pub heights: Vec<PatchCell>,
    /// Library shelf ("Trees", "House parts"); empty for a harvest.
    #[serde(default)]
    pub category: String,
    /// The stock recipe it came from ([`super::stock`]).
    #[serde(default)]
    pub stock: Option<String>,
    /// The library's [`super::stock::REVISION`] when it was made: older stock prefabs are made again.
    #[serde(default)]
    pub stock_rev: u32,
    /// The house workbench design it was built from (screen 23, [`super::design`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<String>,
}

impl Prefab {
    /// Height of its highest vertex above the ground, in units.
    pub fn height(&self) -> i16 {
        self.pool.iter().map(|v| -v[1]).max().unwrap_or(0).max(0)
    }
}

/// What a harvest takes from the area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarvestOptions {
    /// Leave out floor polygons lying on the ground (the terrain under a house).
    pub skip_ground: bool,
    /// Ground surface y; `None` = the most common floor height on the area's border.
    pub ground_y: Option<i16>,
    /// Whole props ([`super::props`]) whose centre lies in the area, without
    /// the terrain; the footprint becomes the cells they cover.
    pub objects: bool,
    /// Only polygons whose centre stands `min..=max` units above the ground
    /// (a house's walls, its roof).
    pub band: Option<[i16; 2]>,
    /// Also polygons centred on the area's east and north edges (walls
    /// standing on the far lines of a module).
    pub far_edges: bool,
    /// Only polygons drawn with one of these CLUTs (a chimney on a roof).
    pub cluts: Vec<u16>,
    /// Palette swaps: tiles drawn with the first CLUT get the second's
    /// colours (a blue roof made orange).
    pub recolour: Vec<[u16; 2]>,
    /// Exactly these polygons, (group, index), wherever the area is (baked
    /// pieces, [`super::unbake`]): the footprint becomes the cells they
    /// cover, and they stand on the terrain around them.
    pub prims: Vec<(usize, usize)>,
}

impl Default for HarvestOptions {
    fn default() -> Self {
        HarvestOptions { skip_ground: true, ground_y: None, objects: false, band: None, far_edges: false, cluts: Vec::new(), recolour: Vec::new(), prims: Vec::new() }
    }
}

fn err(code: &'static str, msg: impl Into<String>) -> EditError {
    EditError::new(code, msg)
}

/// The top floor surface y under each cell centre (the baker's rule, before the −1).
fn floor_tops(c: &Chunk) -> Vec<Option<i16>> {
    bake::surfaces(c)
        .iter()
        .map(|cell| {
            let top = cell.iter().filter(|f| f.flat >= bake::FLOOR_FLAT).map(|f| f.y).fold(f32::INFINITY, f32::min);
            top.is_finite().then(|| top.round() as i16)
        })
        .collect()
}

/// The most common floor y on the cells around `area` (inside the chunk); `None` if none.
pub fn ground_around(c: &Chunk, area: CellRect) -> Option<i16> {
    let tops = floor_tops(c);
    let mut count: HashMap<i16, u32> = HashMap::new();
    let (c0, r0, c1, r1) = (area.c0 as i32 - 1, area.r0 as i32 - 1, area.c1 as i32 + 1, area.r1 as i32 + 1);
    for r in r0..=r1 {
        for col in c0..=c1 {
            let border = r == r0 || r == r1 || col == c0 || col == c1;
            if border && (0..128).contains(&r) && (0..128).contains(&col) {
                if let Some(y) = tops[r as usize * 128 + col as usize] {
                    *count.entry(y).or_default() += 1;
                }
            }
        }
    }
    count.into_iter().max_by_key(|&(y, n)| (n, -(y as i32))).map(|(y, _)| y)
}

/// The chunk's own page: where a tpage word points into it, the page row its band starts at.
fn own_band(t: &Texture, tpage: u16) -> Option<u16> {
    let (tx, ty) = ((tpage & 0xF) * 64, ((tpage >> 4) & 1) * 256);
    (tx == t.image.x && ty >= t.image.y && ty < t.image.y + t.image.h).then(|| ty - t.image.y)
}

fn pixel(t: &Texture, x: usize, y: usize) -> u8 {
    let b = t.pixels[y * t.image.w as usize * 2 + x / 2];
    if x & 1 == 0 {
        b & 15
    } else {
        b >> 4
    }
}

fn set_pixel(t: &mut Texture, x: usize, y: usize, v: u8) {
    let i = y * t.image.w as usize * 2 + x / 2;
    let b = &mut t.pixels[i];
    *b = if x & 1 == 0 { (*b & 0xF0) | v } else { (*b & 0x0F) | v << 4 };
}

/// The 16 entries of CLUT id `clut` in the chunk's CLUT block, if it lies there.
fn palette(t: &Texture, clut: u16) -> Option<Vec<u16>> {
    let (r, e) = t.clut.as_ref()?;
    let (row, col) = ((clut >> 6) as i32 - r.y as i32, (clut & 0x3F) as i32 * 16 - r.x as i32);
    if row < 0 || col < 0 || row >= r.h as i32 || col + 16 > r.w as i32 {
        return None;
    }
    let o = row as usize * r.w as usize + col as usize;
    Some(e[o..o + 16].to_vec())
}

/// Where a polygon's texels come from: the chunk's own page (rows from the
/// page's first row) or a shared 4bpp VRAM page (its tpage page bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Src {
    Own(u16),
    Shared(u16),
}

/// Cuts the polygons whose centroid lies in `area` (with their textures and
/// the walk heights under the area) out of `c` as a prefab. Polygons on
/// shared pages keep them; [`harvest_in`] copies them into the prefab.
pub fn harvest(c: &Chunk, chunk: u32, area: CellRect, name: &str, opts: HarvestOptions) -> Result<Prefab> {
    harvest_in(c, chunk, area, name, &opts, None)
}

/// [`harvest`] with the chunk's field VRAM: 4bpp polygons on shared pages
/// (the field atlas, region sets) get tiles of their own too, so the prefab
/// looks the same in any chunk.
pub fn harvest_in(c: &Chunk, chunk: u32, area: CellRect, name: &str, opts: &HarvestOptions, shared: Option<&Vram>) -> Result<Prefab> {
    if !area.valid() {
        return Err(err("PREFAB_AREA", "The area must lie inside the chunk's 128 × 128 cells."));
    }
    let centroid = |p: &Prim| {
        let n = p.n as usize;
        let vs = p.idx[..n].iter().map(|&i| c.pool[i as usize]);
        let s = vs.fold([0i32; 3], |a, v| [a[0] + v[0] as i32, a[1] + v[1] as i32, a[2] + v[2] as i32]);
        [s[0] / n as i32, s[1] / n as i32, s[2] / n as i32]
    };
    // Whole props: those centred in the area; the footprint becomes the cells they cover.
    // Whole props also note each polygon's object bounds (x0, z0, x1, z1), for finding ground patches.
    let (area, picked, owners): (CellRect, Vec<Prim>, Vec<[i16; 4]>) = if opts.objects {
        let (ox, oz) = area.origin();
        let (x1, z1) = (ox + area.w() as i32 * 32, oz + area.h() as i32 * 32);
        let objs: Vec<super::props::Object> = super::props::objects(c)
            .into_iter()
            .filter(|o| {
                let (cx, cz) = o.centre();
                (ox..x1).contains(&cx) && (oz..z1).contains(&cz)
            })
            .collect();
        if objs.is_empty() {
            return Err(err("PREFAB_EMPTY", "No prop is centred in that area."));
        }
        let cell = |v: i32| ((v + 2048).div_euclid(32)).clamp(0, 127) as u8;
        let (mut a0, mut a1) = ([i16::MAX; 3], [i16::MIN; 3]);
        for o in &objs {
            for k in 0..3 {
                a0[k] = a0[k].min(o.min[k]);
                a1[k] = a1[k].max(o.max[k]);
            }
        }
        let rect = CellRect { c0: cell(a0[0] as i32), r0: cell(a0[2] as i32), c1: cell(a1[0] as i32 - 1).max(cell(a0[0] as i32)), r1: cell(a1[2] as i32 - 1).max(cell(a0[2] as i32)) };
        let prims = objs.iter().flat_map(|o| o.prims.iter().map(|&(g, i)| c.groups[g].prims[i])).collect();
        let owners = objs.iter().flat_map(|o| o.prims.iter().map(|_| [o.min[0], o.min[2], o.max[0], o.max[2]])).collect();
        (rect, prims, owners)
    } else if !opts.prims.is_empty() {
        let prims: Vec<Prim> = opts.prims.iter().filter_map(|&(g, i)| c.groups.get(g)?.prims.get(i).copied()).collect();
        if prims.len() != opts.prims.len() {
            return Err(err("PREFAB_PRIMS", "Some of those polygons aren't in the chunk."));
        }
        let cell = |v: i32| ((v + 2048).div_euclid(32)).clamp(0, 127) as u8;
        let vs = prims.iter().flat_map(|p| p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]));
        let (x0, x1) = (vs.clone().map(|v| v[0] as i32).min().unwrap(), vs.clone().map(|v| v[0] as i32).max().unwrap());
        let (z0, z1) = (vs.clone().map(|v| v[2] as i32).min().unwrap(), vs.map(|v| v[2] as i32).max().unwrap());
        let rect = CellRect { c0: cell(x0), r0: cell(z0), c1: cell(x1 - 1).max(cell(x0)), r1: cell(z1 - 1).max(cell(z0)) };
        (rect, prims, Vec::new())
    } else {
        (area, Vec::new(), Vec::new())
    };
    let whole = opts.objects || !opts.prims.is_empty();
    // Props stand on the terrain around them (other props, such as crates or palm fronds,
    // aren't the ground) unless that lies well below their base: then they stand on
    // something raised (a roof garden), and their base is the ground.
    let ground_y = opts
        .ground_y
        .or_else(|| {
            if !whole {
                return ground_around(c, area);
            }
            let base = picked.iter().flat_map(|p| p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize][1])).max()?;
            let mut bare = c.clone();
            for g in &mut bare.groups {
                g.prims.retain(|p| super::props::is_terrain(c, p));
            }
            Some(ground_around(&bare, area).filter(|&g| g as i32 <= base as i32 + 32).unwrap_or(base))
        })
        .unwrap_or(0);
    // Whole props come without the ground patch they stand on (the grass, sand or snow
    // under a tree or a rock, the dirt mound around a trunk): flat polygons at their foot,
    // and, on tile-sized props, polygons facing up within 20 units of the base (trunks and
    // walls are steep). A prop that is nothing but such a patch (grass tufts) keeps it. The
    // footprint shrinks to the rest.
    let (area, picked) = if opts.objects && opts.skip_ground {
        let vs = |p: &Prim| p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]).collect::<Vec<_>>();
        let base = picked.iter().flat_map(vs).map(|v| v[1] as i32).max().unwrap_or(0);
        let foot = |y: i32| (y - base).abs() <= 4 || (y - ground_y as i32).abs() <= 4;
        // The size of the polygon's own object: a trunk's patch is a tile even where the crown is wider.
        let patch = |(p, &[bx0, bz0, bx1, bz1]): (&Prim, &[i16; 4])| {
            let v = vs(p);
            let (hi, lo) = (v.iter().map(|v| v[1] as i32).max().unwrap(), v.iter().map(|v| v[1] as i32).min().unwrap());
            let flat = hi - lo <= 4 && v.iter().all(|v| foot(v[1] as i32));
            // Facing up (not a low wall), on a prop as big as the patches are (a tile, 128 units).
            let (a, b) = ([v[1][0] - v[0][0], v[1][1] - v[0][1], v[1][2] - v[0][2]], [v[2][0] - v[0][0], v[2][1] - v[0][1], v[2][2] - v[0][2]]);
            let n = [a[1] as f32 * b[2] as f32 - a[2] as f32 * b[1] as f32, a[2] as f32 * b[0] as f32 - a[0] as f32 * b[2] as f32, a[0] as f32 * b[1] as f32 - a[1] as f32 * b[0] as f32];
            let up = n[1].abs() >= 0.8 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let big = bx1 - bx0 >= 96 && bz1 - bz0 >= 96;
            // From the ground (a corner at the foot) up to 20 units: not the top of a low rock.
            let skirt = big && up && base - lo <= 20 && foot(hi);
            flat || skirt
        };
        let kept: Vec<Prim> = picked.iter().zip(&owners).filter(|&q| !patch(q)).map(|(p, _)| *p).collect();
        if kept.is_empty() || kept.len() == picked.len() {
            (area, picked)
        } else {
            let cell = |v: i32| ((v + 2048).div_euclid(32)).clamp(0, 127) as u8;
            let xz = kept.iter().flat_map(|p| p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]));
            let (x0, x1) = (xz.clone().map(|v| v[0] as i32).min().unwrap(), xz.clone().map(|v| v[0] as i32).max().unwrap());
            let (z0, z1) = (xz.clone().map(|v| v[2] as i32).min().unwrap(), xz.map(|v| v[2] as i32).max().unwrap());
            let rect = CellRect { c0: cell(x0), r0: cell(z0), c1: cell(x1 - 1).max(cell(x0)), r1: cell(z1 - 1).max(cell(z0)) };
            (rect, kept)
        }
    } else {
        (area, picked)
    };
    let (ox, oz) = area.origin();
    let (x1, z1) = (ox + area.w() as i32 * 32, oz + area.h() as i32 * 32);
    let t = &c.texture;
    let mut picked = picked;
    if !whole {
        let inside = |v: i32, lo: i32, hi: i32| v >= lo && (v < hi || (opts.far_edges && v == hi));
        for g in &c.groups {
            for p in &g.prims {
                let [cx, cy, cz] = centroid(p);
                if !inside(cx, ox, x1) || !inside(cz, oz, z1) {
                    continue;
                }
                let n = p.n as usize;
                if opts.skip_ground && p.idx[..n].iter().all(|&i| (c.pool[i as usize][1] - ground_y).abs() <= 2) {
                    continue;
                }
                if let Some([lo, hi]) = opts.band {
                    let above = ground_y as i32 - cy;
                    if above < lo as i32 || above > hi as i32 {
                        continue;
                    }
                }
                if !opts.cluts.is_empty() && !(p.textured() && opts.cluts.contains(&p.clut)) {
                    continue;
                }
                picked.push(*p);
            }
        }
    }
    if picked.is_empty() {
        return Err(err("PREFAB_EMPTY", "No polygons above the ground in that area."));
    }
    let recolour = |clut: u16| opts.recolour.iter().find(|r| r[0] == clut).map_or(clut, |r| r[1]);
    // Texture boxes per source page and CLUT, merged where they overlap.
    let src_of = |p: &Prim| -> Option<Src> {
        if !p.textured() {
            return None;
        }
        match own_band(t, p.tpage) {
            Some(b) => Some(Src::Own(b)),
            None if shared.is_some() && (p.tpage >> 7) & 3 == 0 => Some(Src::Shared(p.tpage & 0x1F)),
            None => None,
        }
    };
    // Page key (own = 0xFFFF), CLUT, [x0, y0, x1, y1] inclusive in that page's texels.
    type Key = (u16, u16);
    let key_of = |s: Src, clut: u16| -> Key { (if let Src::Shared(pg) = s { pg } else { 0xFFFF }, clut) };
    let mut boxes: Vec<(Key, [u16; 4])> = Vec::new();
    let mut prim_box: Vec<Option<(Key, [u16; 4])>> = Vec::new();
    for p in &picked {
        match src_of(p) {
            Some(s) => {
                let n = p.n as usize;
                let off = if let Src::Own(b) = s { b } else { 0 };
                let us = p.uv[..n].iter().map(|u| u[0] as u16);
                let vs = p.uv[..n].iter().map(|u| u[1] as u16 + off);
                let bx = [us.clone().min().unwrap(), vs.clone().min().unwrap(), us.max().unwrap(), vs.max().unwrap()];
                let k = key_of(s, p.clut);
                prim_box.push(Some((k, bx)));
                boxes.push((k, bx));
            }
            None => prim_box.push(None),
        }
    }
    let overlaps = |a: &[u16; 4], b: &[u16; 4]| a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3];
    let mut merged: Vec<(Key, [u16; 4])> = Vec::new();
    for (k, bx) in boxes {
        let mut cur = bx;
        while let Some(j) = merged.iter().position(|(k2, b2)| *k2 == k && overlaps(&cur, b2)) {
            let (_, b2) = merged.swap_remove(j);
            cur = [cur[0].min(b2[0]), cur[1].min(b2[1]), cur[2].max(b2[2]), cur[3].max(b2[3])];
        }
        merged.push((k, cur));
    }
    let mut tiles = Vec::with_capacity(merged.len());
    for ((page, clut), b) in &merged {
        let (w, h) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
        let origin = if *page == 0xFFFF {
            // Own page: rows from the page's first row; a box within one 256-row band.
            let (r0, r1) = (t.image.y + b[1], t.image.y + b[3]);
            (r0 >> 8 == r1 >> 8).then(|| TileOrigin { page: t.image.x / 64 | ((r0 >> 8) & 1) << 4, x: b[0], y: r0 & 0xFF, clut: recolour(*clut) })
        } else {
            Some(TileOrigin { page: *page, x: b[0], y: b[1], clut: recolour(*clut) })
        };
        let mut px = String::with_capacity(w as usize * h as usize);
        let pal = if *page == 0xFFFF {
            for y in b[1]..=b[3] {
                for x in b[0]..=b[2] {
                    px.push(char::from_digit(pixel(t, x as usize, y as usize) as u32, 16).unwrap());
                }
            }
            palette(t, recolour(*clut)).ok_or_else(|| err("PREFAB_CLUT", format!("CLUT {:#06x} isn't in this chunk's palette block.", recolour(*clut))))?
        } else {
            let v = shared.expect("shared pages come with a VRAM");
            let (hx, hy) = ((*page as usize & 0xF) * 64, ((*page as usize >> 4) & 1) * 256);
            for y in b[1]..=b[3] {
                for x in b[0]..=b[2] {
                    let hw = v.get(hx + x as usize / 4, hy + y as usize);
                    px.push(char::from_digit(((hw >> ((x & 3) * 4)) & 0xF) as u32, 16).unwrap());
                }
            }
            let cl = recolour(*clut);
            let (cx, cy) = ((cl as usize & 0x3F) * 16, cl as usize >> 6);
            (0..16).map(|i| v.get(cx + i, cy)).collect()
        };
        tiles.push(Tile { w, h, pixels: px, palette: pal, origin });
    }
    // Pool relative to the area's corner and the ground; prims re-indexed.
    let mut pool: Vec<[i16; 3]> = Vec::new();
    let mut remap: HashMap<u16, u16> = HashMap::new();
    let mut prims = Vec::with_capacity(picked.len());
    for (p, pb) in picked.iter().zip(&prim_box) {
        let mut q = *p;
        for k in 0..p.n as usize {
            let i = p.idx[k];
            q.idx[k] = *remap.entry(i).or_insert_with(|| {
                let v = c.pool[i as usize];
                pool.push([(v[0] as i32 - ox) as i16, v[1] - ground_y, (v[2] as i32 - oz) as i16]);
                (pool.len() - 1) as u16
            });
        }
        let tile = pb.map(|(k, b)| {
            let j = merged.iter().position(|(k2, mb)| *k2 == k && overlaps(mb, &b)).unwrap();
            let (_, mb) = merged[j];
            let off = if k.0 == 0xFFFF { own_band(t, p.tpage).unwrap() } else { 0 };
            for uv in &mut q.uv[..p.n as usize] {
                *uv = [(uv[0] as u16 - mb[0]) as u8, (uv[1] as u16 + off - mb[1]) as u8];
            }
            j as u16
        });
        prims.push(PrefabPrim { prim: q.into(), tile });
    }
    // Walk heights under the area, relative to the ground's walk height.
    let ground_h = ground_y as i32 - 1;
    let mut heights = Vec::with_capacity(area.w() * area.h());
    for r in area.r0..=area.r1 {
        for col in area.c0..=area.c1 {
            let h = c.heights[r as usize * 128 + col as usize];
            heights.push(if h == -1 { PatchCell::Blocked } else { PatchCell::Rel((h as i32 - ground_h) as i16) });
        }
    }
    Ok(Prefab { name: name.to_string(), source_chunk: chunk, area, ground_y, pool, prims, tiles, heights, category: String::new(), stock: None, stock_rev: 0, design: None })
}

/* ───────────────────────── Placement ───────────────────────── */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    /// Cell of the placed footprint's min corner.
    pub col: u8,
    pub row: u8,
    /// Quarter turns, counter-clockwise in the north-up top view.
    pub rot: u8,
    /// Ground surface y at the destination; `None` = the most common floor around the footprint.
    pub ground_y: Option<i16>,
    /// Also write the patch's ground-level cells (flattening the terrain under the footprint).
    pub write_ground: bool,
    /// Units the prefab stands above the ground (a roof on walls). Walk
    /// surfaces at or above the ground rise with it; pits stay.
    #[serde(default)]
    pub lift: i16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placed {
    pub groups: u32,
    pub vertices: u32,
    pub polygons: u32,
    pub tiles: u32,
    pub cluts_reused: u32,
    pub cluts_new: u32,
    /// Walk cells written.
    pub cells: u32,
    /// Polygons that sample a shared page (right only where it is loaded).
    pub shared_texture: u32,
    pub footprint: CellRect,
    /// The ground it stands on (PSX Y).
    pub ground_y: i16,
}

/// Footprint size after `rot` quarter turns.
pub fn footprint(p: &Prefab, rot: u8) -> (usize, usize) {
    if rot % 2 == 0 {
        (p.area.w(), p.area.h())
    } else {
        (p.area.h(), p.area.w())
    }
}

/// Texture tiles already packed into a chunk's page during one build (a
/// scene's rebuild, a [`compose`]): a tile with the same pixels reuses the
/// spot, so ten pines share one copy of their texture.
#[derive(Clone, Default)]
pub struct PackCache {
    spots: HashMap<(u16, u16, String), (u16, u16)>,
    /// The chunk's field VRAM (atlas, region set, sibling pages): tiles
    /// whose texels and palette it holds where they were cut are sampled
    /// there ([`TileOrigin`]).
    pub vram: Option<std::sync::Arc<Vram>>,
}

impl PackCache {
    pub fn with_vram(vram: Vram) -> Self {
        PackCache { spots: HashMap::new(), vram: Some(std::sync::Arc::new(vram)) }
    }
}

/// Where a tile's texels went: a spot on the chunk's own page, or its
/// origin on another loaded page.
#[derive(Debug, Clone, Copy)]
struct Slot {
    x: u16,
    y: u16,
    clut: u16,
    /// Another page (its tpage page bits): `x`, `y` are texels on it.
    page: Option<u16>,
}

/// Puts `p` into `c`. Fails without changing `c` when the footprint leaves
/// the chunk or the textures don't fit.
pub fn place(c: &mut Chunk, p: &Prefab, at: Placement) -> Result<Placed> {
    place_with(c, p, at, &mut PackCache::default())
}

/// [`place`], sharing texture tiles with the placements `cache` saw in `c`.
pub fn place_with(c: &mut Chunk, p: &Prefab, at: Placement, cache: &mut PackCache) -> Result<Placed> {
    let rot = at.rot % 4;
    let (fw, fh) = footprint(p, rot);
    if at.col as usize + fw > 128 || at.row as usize + fh > 128 {
        return Err(err("PREFAB_OUT_OF_CHUNK", format!("A {fw} × {fh}-cell footprint at cell {}, {} runs past the chunk's edge.", at.col, at.row)));
    }
    let area = CellRect { c0: at.col, r0: at.row, c1: at.col + fw as u8 - 1, r1: at.row + fh as u8 - 1 };
    let ground_y = at.ground_y.or_else(|| ground_around(c, area)).unwrap_or(0);
    let mut out = c.clone();
    let (w_units, h_units) = (p.area.w() as i32 * 32, p.area.h() as i32 * 32);
    let (dx, dz) = area.origin();
    let turn = |x: i32, z: i32| -> (i32, i32) {
        match rot {
            0 => (x, z),
            1 => (h_units - z, x),
            2 => (w_units - x, h_units - z),
            _ => (z, w_units - x),
        }
    };
    // Vertices.
    let base = out.pool.len();
    if base + p.pool.len() > MAX_VERTICES {
        return Err(err("CHUNK_VERTICES", format!("The chunk would hold {} vertices; the limit is {MAX_VERTICES}.", base + p.pool.len())));
    }
    for v in &p.pool {
        let (x, z) = turn(v[0] as i32, v[2] as i32);
        let to = |a: i32| a.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out.pool.push([to(dx + x), to(ground_y as i32 + v[1] as i32 - at.lift as i32), to(dz + z)]);
    }
    // Textures: pack each tile into free space on the page, palettes into the CLUT block.
    let mut packed = cache.clone();
    let (slots, reused, created) = pack_tiles(&mut out.texture, &c.groups, &p.tiles, &mut packed)?;
    // Polygons, split into 512-unit draw groups so culling stays tight.
    let mut groups: Vec<((i32, i32), Group)> = Vec::new();
    let mut shared = 0;
    for pp in &p.prims {
        let mut q: Prim = pp.prim.into();
        for k in 0..q.n as usize {
            q.idx[k] = (base + q.idx[k] as usize) as u16;
        }
        match pp.tile.map(|t| slots[t as usize]) {
            Some(Slot { x, y, clut, page: Some(page) }) => {
                for uv in &mut q.uv[..q.n as usize] {
                    *uv = [(x + uv[0] as u16) as u8, (y + uv[1] as u16) as u8];
                }
                q.tpage = page | (pp.prim.tpage & 0x60);
                q.clut = clut;
            }
            Some(Slot { x: x0, y: y0, clut, page: None }) => {
                let row0 = out.texture.image.y as u32 + y0 as u32;
                for uv in &mut q.uv[..q.n as usize] {
                    let y = row0 + uv[1] as u32;
                    *uv = [(x0 + uv[0] as u16) as u8, (y & 0xFF) as u8];
                }
                let band = ((row0 + pp.prim.uv[..q.n as usize].iter().map(|u| u[1] as u32).min().unwrap()) >> 8) & 1;
                q.tpage = out.texture.image.x / 64 | (band as u16) << 4 | (pp.prim.tpage & 0x60);
                q.clut = clut;
            }
            None if q.textured() => shared += 1,
            None => {}
        }
        let n = q.n as usize;
        let cx = q.idx[..n].iter().map(|&i| out.pool[i as usize][0] as i32).sum::<i32>() / n as i32;
        let cz = q.idx[..n].iter().map(|&i| out.pool[i as usize][2] as i32).sum::<i32>() / n as i32;
        let key = ((cx + 2048).div_euclid(512), (cz + 2048).div_euclid(512));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.prims.push(q),
            None => groups.push((key, Group { centre: [0; 3], prims: vec![q] })),
        }
    }
    let first = out.groups.len();
    out.groups.extend(groups.into_iter().map(|(_, g)| g));
    for g in first..out.groups.len() {
        out.recompute_centre(g);
    }
    // Walk heights: the patch turned with the geometry, relative to the destination ground.
    let ground_h = ground_y as i32 - 1;
    let mut cells = 0;
    for j in 0..p.area.h() {
        for i in 0..p.area.w() {
            let cell = p.heights[j * p.area.w() + i];
            if !at.write_ground && cell == PatchCell::Rel(0) {
                continue;
            }
            let (ti, tj) = match rot {
                0 => (i, j),
                1 => (p.area.h() - 1 - j, i),
                2 => (p.area.w() - 1 - i, p.area.h() - 1 - j),
                _ => (j, p.area.w() - 1 - i),
            };
            let k = (at.row as usize + tj) * 128 + at.col as usize + ti;
            out.heights[k] = match cell {
                PatchCell::Blocked => -1,
                PatchCell::Rel(r) => (ground_h + r as i32 - if r <= 0 { at.lift as i32 } else { 0 }).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            };
            cells += 1;
        }
    }
    let placed = Placed {
        groups: (out.groups.len() - first) as u32,
        vertices: p.pool.len() as u32,
        polygons: p.prims.len() as u32,
        tiles: p.tiles.len() as u32,
        cluts_reused: reused,
        cluts_new: created,
        cells,
        shared_texture: shared,
        footprint: area,
        ground_y,
    };
    out.sizes()?;
    *c = out;
    *cache = packed;
    Ok(placed)
}

/// Packs `tiles` into free space on `t`'s page (texels no own-page polygon of
/// `groups` samples) and their palettes into the CLUT block. Returns, per
/// tile, its page x, page row and CLUT id, plus palettes reused and written.
fn pack_tiles(t: &mut Texture, groups: &[Group], tiles: &[Tile], cache: &mut PackCache) -> Result<(Vec<Slot>, u32, u32)> {
    let (pw, ph) = (t.image.w as usize * 4, t.image.h as usize);
    let mut used = vec![false; pw * ph];
    let mut cluts_in_use: Vec<u16> = Vec::new();
    for g in groups {
        for p in &g.prims {
            if !p.textured() {
                continue;
            }
            cluts_in_use.push(p.clut);
            let Some(band) = own_band(t, p.tpage) else { continue };
            let n = p.n as usize;
            let (u0, u1) = (p.uv[..n].iter().map(|u| u[0]).min().unwrap() as usize, p.uv[..n].iter().map(|u| u[0]).max().unwrap() as usize);
            let (v0, v1) = (p.uv[..n].iter().map(|u| u[1]).min().unwrap() as usize, p.uv[..n].iter().map(|u| u[1]).max().unwrap() as usize);
            for y in v0 + band as usize..=(v1 + band as usize).min(ph - 1) {
                for x in u0..=u1.min(pw - 1) {
                    used[y * pw + x] = true;
                }
            }
        }
    }
    let (crect, centries) = t.clut.clone().ok_or_else(|| err("PREFAB_NO_CLUT", "This chunk's texture page has no palettes."))?;
    let slot_id = |s: usize| -> u16 {
        let per_row = crect.w as usize / 16;
        let (row, col) = (s / per_row, s % per_row);
        ((crect.y as usize + row) << 6 | (crect.x as usize + col * 16) / 16) as u16
    };
    let nslots = (crect.w as usize / 16) * crect.h as usize;
    let mut entries = centries;
    let mut taken: Vec<bool> = (0..nslots)
        .map(|s| {
            let o = (s / (crect.w as usize / 16)) * crect.w as usize + (s % (crect.w as usize / 16)) * 16;
            cluts_in_use.contains(&slot_id(s)) || entries[o..o + 16].iter().any(|&e| e != 0)
        })
        .collect();
    let (mut reused, mut created) = (0, 0);
    // Largest tiles first; first fit in raster order with a summed-area table for the free test.
    let mut order: Vec<usize> = (0..tiles.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(tiles[i].w as usize * tiles[i].h as usize));
    let mut out = vec![Slot { x: 0, y: 0, clut: 0, page: None }; tiles.len()];
    let digit = |tile: &Tile, k: usize| (tile.pixels.as_bytes()[k] as char).to_digit(16).unwrap_or(0) as u16;
    for i in order {
        let tile = &tiles[i];
        let (tw, th) = (tile.w as usize, tile.h as usize);
        if tw > pw || th > ph || th > 256 {
            return Err(err("PREFAB_TEXTURE_FULL", format!("A {tw} × {th} texture tile is larger than this chunk's page.")));
        }
        // Already loaded where it was cut: another page with the same texels and palette.
        if let (Some(o), Some(v)) = (tile.origin, cache.vram.as_deref()) {
            let (hx, hy) = ((o.page as usize & 0xF) * 64, ((o.page as usize >> 4) & 1) * 256);
            let on_own = hx == t.image.x as usize;
            let texels = (0..th).all(|yy| {
                (0..tw).all(|xx| {
                    let (u, w) = (o.x as usize + xx, o.y as usize + yy);
                    u < 256 && w < 256 && (v.get(hx + u / 4, hy + w) >> ((u & 3) * 4)) & 0xF == digit(tile, yy * tw + xx)
                })
            });
            let (cx, cy) = ((o.clut as usize & 0x3F) * 16, o.clut as usize >> 6);
            let palette = (0..16).all(|k| v.get(cx + k, cy) == tile.palette[k]);
            if !on_own && texels && palette {
                out[i] = Slot { x: o.x, y: o.y, clut: o.clut, page: Some(o.page) };
                reused += 1;
                continue;
            }
        }
        // On this chunk's own page already, where polygons sample it (so nothing packs over it).
        let own_spot = tile.origin.and_then(|o| {
            let row = (((o.page >> 4) & 1) * 256 + o.y) as i32 - t.image.y as i32;
            let fits = (o.page & 0xF) * 64 == t.image.x && row >= 0 && row as usize + th <= ph && o.x as usize + tw <= pw;
            let here = fits
                && (0..th).all(|yy| {
                    (0..tw).all(|xx| {
                        let (x, y) = (o.x as usize + xx, row as usize + yy);
                        used[y * pw + x] && pixel(t, x, y) as u16 == digit(tile, yy * tw + xx)
                    })
                });
            here.then_some((o.x as usize, row as usize))
        });
        let key = (tile.w, tile.h, tile.pixels.clone());
        let (x, y) = match own_spot.or_else(|| cache.spots.get(&key).map(|&(x, y)| (x as usize, y as usize))) {
            Some(spot) => spot,
            None => {
                let sat = summed(&used, pw, ph);
                let busy = |x: usize, y: usize| sat[(y + th) * (pw + 1) + x + tw] + sat[y * (pw + 1) + x] - sat[y * (pw + 1) + x + tw] - sat[(y + th) * (pw + 1) + x];
                let spot = (0..=ph.saturating_sub(th))
                    .filter(|&y| (t.image.y as usize + y) >> 8 == (t.image.y as usize + y + th - 1) >> 8)
                    .find_map(|y| (0..=pw.saturating_sub(tw)).find(|&x| busy(x, y) == 0).map(|x| (x, y)));
                let Some((x, y)) = spot else {
                    return Err(err("PREFAB_TEXTURE_FULL", format!("No free {tw} × {th} area left on this chunk's texture page.")));
                };
                for yy in 0..th {
                    for xx in 0..tw {
                        let v = tile.pixels.as_bytes()[yy * tw + xx];
                        set_pixel(t, x + xx, y + yy, (v as char).to_digit(16).unwrap_or(0) as u8);
                        used[(y + yy) * pw + x + xx] = true;
                    }
                }
                cache.spots.insert(key, (x as u16, y as u16));
                (x, y)
            }
        };
        // Palette: an identical slot, else a free one.
        let row_of = |s: usize| (s / (crect.w as usize / 16)) * crect.w as usize + (s % (crect.w as usize / 16)) * 16;
        let same = (0..nslots).find(|&s| taken[s] && entries[row_of(s)..row_of(s) + 16] == tile.palette[..]);
        let s = match same {
            Some(s) => {
                reused += 1;
                s
            }
            None => {
                let s = (0..nslots).find(|&s| !taken[s]).ok_or_else(|| err("PREFAB_CLUT_FULL", "No free palette slot left in this chunk."))?;
                let o = row_of(s);
                entries[o..o + 16].copy_from_slice(&tile.palette);
                taken[s] = true;
                created += 1;
                s
            }
        };
        out[i] = Slot { x: x as u16, y: y as u16, clut: slot_id(s), page: None };
    }
    t.clut = Some((crect, entries));
    Ok((out, reused, created))
}

/// One prefab in a [`compose`]: where its footprint's min corner goes,
/// turned and lifted as [`place`] does.
#[derive(Debug, Clone, Copy)]
pub struct Part<'a> {
    pub prefab: &'a Prefab,
    pub col: u8,
    pub row: u8,
    pub rot: u8,
    pub lift: i16,
}

/// Puts `parts` together into one prefab (a house from its modules) on flat
/// ground: placed in order into a blank chunk, then harvested back over the
/// cells their footprints cover. Later parts overwrite earlier walk cells.
pub fn compose(parts: &[Part], name: &str) -> Result<Prefab> {
    let first = parts.first().ok_or_else(|| err("PREFAB_EMPTY", "Nothing to put together."))?;
    let mut c = Chunk::blank(0);
    c.texture.image.h = 512;
    c.texture.pixels = vec![0; 64 * 512 * 2];
    if let Some((r, e)) = &mut c.texture.clut {
        r.h = 32;
        *e = vec![0; 16 * 32];
    }
    c.heights = vec![-1; CELLS];
    let (mut c0, mut r0, mut c1, mut r1) = (u8::MAX, u8::MAX, 0u8, 0u8);
    let mut cache = PackCache::default();
    for p in parts {
        let at = Placement { col: p.col, row: p.row, rot: p.rot, ground_y: Some(0), write_ground: false, lift: p.lift };
        let placed = place_with(&mut c, p.prefab, at, &mut cache)?;
        let f = placed.footprint;
        (c0, r0, c1, r1) = (c0.min(f.c0), r0.min(f.r0), c1.max(f.c1), r1.max(f.r1));
    }
    let opts = HarvestOptions { skip_ground: false, ground_y: Some(0), far_edges: true, ..HarvestOptions::default() };
    let mut out = harvest(&c, first.prefab.source_chunk, CellRect { c0, r0, c1, r1 }, name, opts)?;
    out.category = first.prefab.category.clone();
    // The harvest's origins are on the scratch page: point them back where the parts' tiles came from.
    let image = c.texture.image;
    for t in &mut out.tiles {
        let Some(o) = t.origin else { continue };
        let (ox, oy) = (o.x, ((o.page >> 4) & 1) * 256 + o.y - image.y);
        t.origin = parts.iter().flat_map(|p| p.prefab.tiles.iter()).find_map(|pt| {
            let po = pt.origin?;
            let &(sx, sy) = cache.spots.get(&(pt.w, pt.h, pt.pixels.clone()))?;
            let inside = ox >= sx && oy >= sy && ox + t.w <= sx + pt.w && oy + t.h <= sy + pt.h && pt.palette == t.palette;
            inside.then(|| TileOrigin { page: po.page, x: po.x + ox - sx, y: po.y + oy - sy, clut: po.clut })
        });
    }
    Ok(out)
}

/// A top-down picture of the prefab (unturned, north up), `px` pixels per
/// cell: the footprint exactly, so the editor can lay it in a placement
/// ghost. Polygons on shared pages come out untextured. PNG bytes.
pub fn preview(p: &Prefab, px: usize) -> Option<Vec<u8>> {
    let mut c = Chunk::blank(0);
    c.texture.image.h = 512;
    c.texture.pixels = vec![0; 64 * 512 * 2];
    let (w, h) = (p.area.w(), p.area.h());
    place(&mut c, p, Placement { col: 0, row: 0, rot: 0, ground_y: Some(0), write_ground: false, lift: 0 }).ok()?;
    let bytes = c.serialize().ok()?;
    let mesh = crate::world::mesh(&bytes)?;
    let mut vram = crate::vram::Vram::new();
    vram.paint_tim(&bytes[crate::world::TIM_OFFSET..]);
    let (cw, ch) = (w * px, h * px);
    let mut cv = crate::raster::Canvas::new(cw, ch, [0, 0, 0, 0]);
    let scale = px as f32 / 32.0;
    let ortho = crate::raster::Ortho { scale, left: -2048.0, top: -2048.0 + h as f32 * 32.0 };
    crate::raster::draw_mesh(&mut cv, &mesh, Some(&vram), ortho, crate::raster::Style::Textured);
    super::obj::encode_png(cw as u32, ch as u32, &cv.rgba)
}

/// A picture of the prefab from the south-west, 35° down, fitted into a
/// `size`² transparent square: what the library shows. PNG bytes.
pub fn thumbnail(p: &Prefab, size: usize) -> Option<Vec<u8>> {
    thumbnail_turned(p, size, 0)
}

/// [`thumbnail`] of `p` turned `rot` quarter turns first: the other sides.
pub fn thumbnail_turned(p: &Prefab, size: usize, rot: u8) -> Option<Vec<u8>> {
    let mut c = Chunk::blank(0);
    c.full_page();
    place(&mut c, p, Placement { col: 0, row: 0, rot, ground_y: Some(0), write_ground: false, lift: 0 }).ok()?;
    let bytes = c.serialize().ok()?;
    let mut mesh = crate::world::mesh(&bytes)?;
    let (s, k) = (std::f32::consts::FRAC_PI_4.sin(), std::f32::consts::FRAC_PI_4.cos());
    let e = 35f32.to_radians();
    // The rasterizer looks down Y: x stays across, depth goes in y, screen up in z.
    for v in &mut mesh.pool {
        let (x, y, z) = (v[0] as f32, v[1] as f32, v[2] as f32);
        let (rx, rz) = (x * k - z * s, x * s + z * k);
        *v = [rx.round() as i16, (y * e.sin() + rz * e.cos()).round() as i16, (-y * e.cos() + rz * e.sin()).round() as i16];
    }
    let mut vram = crate::vram::Vram::new();
    vram.paint_tim(&bytes[crate::world::TIM_OFFSET..]);
    let mut cv = crate::raster::Canvas::new(size, size, [0, 0, 0, 0]);
    crate::raster::draw_mesh(&mut cv, &mesh, Some(&vram), crate::raster::Ortho::fit(&mesh, size), crate::raster::Style::Textured);
    super::obj::encode_png(size as u32, size as u32, &cv.rgba)
}

fn summed(used: &[bool], w: usize, h: usize) -> Vec<u32> {
    let mut s = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0;
        for x in 0..w {
            row += used[y * w + x] as u32;
            s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×2-cell textured box standing on ground y = −128 in chunk slot 0.
    fn chunk_with_box() -> Chunk {
        let mut c = Chunk::blank(0);
        // Ground quad over cells 60..68 and a 64-high box top over cells 64..65.
        c.pool = vec![
            [-128, -128, -128], [128, -128, -128], [-128, -128, 128], [128, -128, 128],
            [0, -192, 0], [64, -192, 0], [0, -192, 64], [64, -192, 64],
        ];
        let ground = Prim { list: 2, code: 0x28, n: 4, idx: [0, 1, 2, 3], rgb: [[90; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        let top = Prim { list: 3, code: 0x2C, n: 4, idx: [4, 5, 6, 7], rgb: [[128; 3]; 4], uv: [[0, 0], [7, 0], [0, 7], [7, 7]], clut: 0x7802, tpage: 0x0A | 0x20 };
        c.groups.push(Group { centre: [0; 3], prims: vec![ground, top] });
        // Its texture: an 8×8 checker of indices 1/2 at page (0,0); palette 0x7802 = CLUT row 480, x 32.
        for y in 0..8 {
            for x in 0..8 {
                set_pixel(&mut c.texture, x, y, 1 + ((x + y) % 2) as u8);
            }
        }
        if let Some((_, e)) = &mut c.texture.clut {
            e[1] = 0x001F;
            e[2] = 0x7C00;
        }
        c.heights.iter_mut().for_each(|h| *h = -129);
        for (col, row) in [(64, 64), (65, 64), (64, 65), (65, 65)] {
            c.heights[row * 128 + col] = -193;
        }
        c
    }

    #[test]
    fn harvest_takes_the_box_its_texture_and_heights() {
        let c = chunk_with_box();
        let area = CellRect { c0: 63, r0: 63, c1: 66, r1: 66 };
        let p = harvest(&c, 0, area, "box", HarvestOptions::default()).unwrap();
        assert_eq!((p.ground_y, p.prims.len(), p.tiles.len(), p.pool.len()), (-128, 1, 1, 4));
        assert_eq!((p.tiles[0].w, p.tiles[0].h, &p.tiles[0].pixels[..4]), (8, 8, "1212"));
        assert_eq!(p.tiles[0].palette[1..3], [0x001F, 0x7C00]);
        // Vertices relative to the area corner (x −32, z −32) and the ground.
        assert_eq!(p.pool[0], [32, -64, 32]);
        // Heights: the box cells are 64 above the ground's walk height, the rest ground.
        assert_eq!(p.heights[4 + 1], PatchCell::Rel(-64));
        assert_eq!(p.heights[0], PatchCell::Rel(0));
    }

    #[test]
    fn a_polygon_list_takes_just_those_polygons_wherever_the_area_is() {
        let c = chunk_with_box();
        let opts = HarvestOptions { skip_ground: false, prims: vec![(0, 1)], ..HarvestOptions::default() };
        let p = harvest(&c, 0, CellRect { c0: 0, r0: 0, c1: 127, r1: 127 }, "top", opts).unwrap();
        // The box top alone: its own cells, standing on its base (the ground lies 64 below it).
        assert_eq!((p.area, p.prims.len(), p.ground_y), (CellRect { c0: 64, r0: 64, c1: 65, r1: 65 }, 1, -192));
        let bad = HarvestOptions { prims: vec![(0, 9)], ..HarvestOptions::default() };
        assert_eq!(harvest(&c, 0, CellRect { c0: 0, r0: 0, c1: 127, r1: 127 }, "x", bad).unwrap_err().code, "PREFAB_PRIMS");
    }

    /// The box prefab without its origin: placed as if into another chunk, so its tile is packed.
    fn copied(c: &Chunk) -> Prefab {
        let mut p = harvest(c, 0, CellRect { c0: 63, r0: 63, c1: 66, r1: 66 }, "box", HarvestOptions::default()).unwrap();
        p.tiles.iter_mut().for_each(|t| t.origin = None);
        p
    }

    #[test]
    fn a_tile_already_on_the_page_is_sampled_where_it_is() {
        let c = chunk_with_box();
        let p = harvest(&c, 0, CellRect { c0: 63, r0: 63, c1: 66, r1: 66 }, "box", HarvestOptions::default()).unwrap();
        assert_eq!(p.tiles[0].origin, Some(TileOrigin { page: 0x0A, x: 0, y: 0, clut: 0x7802 }));
        let mut d = c.clone();
        let r = place(&mut d, &p, Placement { col: 10, row: 20, rot: 0, ground_y: Some(-64), write_ground: false, lift: 0 }).unwrap();
        let q = d.groups.last().unwrap().prims[0];
        assert_eq!((q.uv, q.clut, q.tpage, r.cluts_new), ([[0, 0], [7, 0], [0, 7], [7, 7]], 0x7802, 0x0A | 0x20, 0));
    }

    #[test]
    fn place_moves_turns_packs_and_patches() {
        let c = chunk_with_box();
        let p = copied(&c);
        let mut d = c.clone();
        let r = place(&mut d, &p, Placement { col: 10, row: 20, rot: 1, ground_y: Some(-64), write_ground: false, lift: 0 }).unwrap();
        assert_eq!((r.polygons, r.tiles, r.cluts_reused, r.cluts_new, r.cells), (1, 1, 1, 0, 4));
        let q = d.groups.last().unwrap().prims[0];
        // The tile went to free space (not over the existing checker at 0..7) with the same palette.
        assert!(q.uv[0][0] >= 8 || q.uv[0][1] >= 8, "{:?}", q.uv);
        assert_eq!((q.clut, q.tpage), (0x7802, 0x0A | 0x20));
        let (u, v) = (q.uv[0][0] as usize, q.uv[0][1] as usize);
        assert_eq!((pixel(&d.texture, u, v), pixel(&d.texture, u + 1, v)), (1, 2));
        // Heights 64 above the new ground (−64 → walk −65 → box −129), turned with the footprint.
        let k = |col: usize, row: usize| d.heights[row * 128 + col];
        assert_eq!((k(11, 21), k(12, 22), k(10, 20)), (-129, -129, -129 /* untouched retail */));
        // Every vertex landed inside the footprint (cells 10..13 × 20..23).
        let (x0, z0) = (10 * 32 - 2048, 20 * 32 - 2048);
        for &i in &q.idx {
            let v = d.pool[i as usize];
            assert!((x0..x0 + 128).contains(&(v[0] as i32)) && (z0..z0 + 128).contains(&(v[2] as i32)) && v[1] == -128, "{v:?}");
        }
        assert!(d.serialize().is_ok());
    }

    #[test]
    fn placements_in_one_build_share_their_texture() {
        let c = chunk_with_box();
        let p = copied(&c);
        let mut d = c.clone();
        let mut cache = PackCache::default();
        let at = |col: u8| Placement { col, row: 20, rot: 0, ground_y: Some(-64), write_ground: false, lift: 0 };
        place_with(&mut d, &p, at(10), &mut cache).unwrap();
        place_with(&mut d, &p, at(20), &mut cache).unwrap();
        let (a, b) = (d.groups[d.groups.len() - 2].prims[0], d.groups[d.groups.len() - 1].prims[0]);
        assert_eq!((a.uv, a.tpage, a.clut), (b.uv, b.tpage, b.clut));
        // Without the cache the second copy goes elsewhere on the page.
        let mut e = c.clone();
        place(&mut e, &p, at(10)).unwrap();
        place(&mut e, &p, at(20)).unwrap();
        assert_ne!(e.groups[e.groups.len() - 2].prims[0].uv, e.groups[e.groups.len() - 1].prims[0].uv);
    }

    #[test]
    fn a_lift_raises_the_geometry_and_the_surfaces_on_it() {
        let c = chunk_with_box();
        let p = harvest(&c, 0, CellRect { c0: 63, r0: 63, c1: 66, r1: 66 }, "box", HarvestOptions::default()).unwrap();
        let mut d = c.clone();
        place(&mut d, &p, Placement { col: 10, row: 20, rot: 0, ground_y: Some(-128), write_ground: false, lift: 100 }).unwrap();
        let q = d.groups.last().unwrap().prims[0];
        assert!(q.idx.iter().all(|&i| d.pool[i as usize][1] == -292), "{:?}", q.idx.map(|i| d.pool[i as usize]));
        assert_eq!(d.heights[21 * 128 + 11], -129 - 64 - 100);
    }

    #[test]
    fn compose_puts_parts_together_as_one_prefab() {
        let c = chunk_with_box();
        let p = harvest(&c, 0, CellRect { c0: 63, r0: 63, c1: 66, r1: 66 }, "box", HarvestOptions::default()).unwrap();
        let both = compose(&[Part { prefab: &p, col: 0, row: 0, rot: 0, lift: 0 }, Part { prefab: &p, col: 4, row: 0, rot: 2, lift: 64 }], "pair").unwrap();
        assert_eq!((both.area.w(), both.area.h(), both.prims.len(), both.tiles.len()), (8, 4, 2, 1));
        assert_eq!(both.height(), 64 + 64);
    }

    #[test]
    fn a_preview_is_the_footprint_in_pixels() {
        let c = chunk_with_box();
        let p = harvest(&c, 0, CellRect { c0: 63, r0: 63, c1: 66, r1: 66 }, "box", HarvestOptions::default()).unwrap();
        let png = preview(&p, 8).unwrap();
        let d = png::Decoder::new(std::io::Cursor::new(png));
        let r = d.read_info().unwrap();
        assert_eq!((r.info().width, r.info().height), (32, 32));
        let thumb = thumbnail(&p, 96).unwrap();
        let r = png::Decoder::new(std::io::Cursor::new(thumb)).read_info().unwrap();
        assert_eq!((r.info().width, r.info().height), (96, 96));
    }

    #[test]
    fn a_full_page_or_a_bad_spot_is_refused() {
        let c = chunk_with_box();
        let p = copied(&c);
        let mut d = c.clone();
        assert_eq!(place(&mut d, &p, Placement { col: 126, row: 0, rot: 0, ground_y: None, write_ground: false, lift: 0 }).unwrap_err().code, "PREFAB_OUT_OF_CHUNK");
        // Fill the page with one polygon that samples all of it.
        let all = Prim { list: 3, code: 0x2C, n: 4, idx: [4, 5, 6, 7], rgb: [[128; 3]; 4], uv: [[0, 0], [255, 0], [0, 255], [255, 255]], clut: 0x7802, tpage: 0x0A };
        d.groups[0].prims.push(all);
        assert_eq!(place(&mut d, &p, Placement { col: 0, row: 0, rot: 0, ground_y: None, write_ground: false, lift: 0 }).unwrap_err().code, "PREFAB_TEXTURE_FULL");
        assert_eq!(d.groups.len(), 1, "a refused placement leaves the chunk alone");
    }
}
