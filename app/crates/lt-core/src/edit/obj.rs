//! World chunk geometry ↔ Wavefront OBJ (screen 10, PRD 10 §5.3), for
//! editing a chunk in Blender. The conventions are `gc_level.py`'s, extended
//! so that exporting any retail chunk and importing the OBJ unchanged gives
//! the same bytes (`tests/chunks.rs`, all 524):
//!
//! - `v x y z r g b`: chunk-local units, Y up (PSX Y negated), colours 0–1
//!   (128/255 = 1.0 modulation). One OBJ vertex per (position, colour); the
//!   pool's vertices come first, in pool order, with their first colour.
//! - `g groupNN`: draw groups in order. Quads are 4-corner faces in loop
//!   order (the PSX strip order 0-1-3-2 is restored on import).
//! - Materials: `cXXXX` = the chunk's own page through CLUT id XXXX,
//!   `tYYYY_cXXXX` = another page (tpage word YYYY), `flat` = untextured.
//!   Suffixes: `_aN` own-page blend mode N (tpage bits 5–6, kept even on
//!   opaque faces), `_st` semi-transparent, `_raw` raw texture, `_g` keep a
//!   gouraud list although the colours are uniform.
//! - `vt`: own page `((u + .5) / 256, 1 − (y + .5) / h)` with y counted from
//!   the page's top row and h its height; other pages use h = 256.
//!
//! Import keeps the base chunk's texture page and both layers; only the
//! draw groups and the pool are replaced. `meta.json` supplies the culling
//! centres; a group that has none, or whose bounding box changed since the
//! export (it was moved or reshaped), gets its bounding-box middle.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::tim::bgr555_to_rgba;
use crate::world::Prim;

use super::chunk::{Chunk, Group, Sizes, Texture};
use super::{EditError, Result};

/// The files of an exported chunk: `map.obj`, `map.mtl`, `meta.json` and
/// one `cXXXX.png` preview per own-page CLUT the faces use.
#[derive(Debug, Clone)]
pub struct Exported {
    pub obj: String,
    pub mtl: String,
    pub meta: String,
    pub images: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub chunk: u32,
    pub groups: Vec<MetaGroup>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaGroup {
    pub name: String,
    pub center: [i16; 3],
    /// Bounding box at export (min x, y, z, max x, y, z; PSX Y down).
    #[serde(default)]
    pub bbox: Option<[i16; 6]>,
}

/// What an import made.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub groups: u32,
    pub polygons: u32,
    pub vertices: u32,
    /// Groups whose centre came from their bounding box (no `meta.json`
    /// entry, or moved or reshaped since the export).
    pub recentred: u32,
    pub sizes: Sizes,
}

/// The chunk's own page: VRAM x of its tpage, first row, height in rows.
struct Page {
    x: u16,
    y: u16,
    h: u16,
}

impl Page {
    fn of(t: &Texture) -> Self {
        Page { x: t.image.x, y: t.image.y, h: t.image.h.max(1) }
    }
    /// The row (from the page's top) a tpage word's 256-row band starts at,
    /// when the word points into this page.
    fn row_of(&self, tpage: u16) -> Option<u16> {
        let (tx, ty) = ((tpage & 0xF) * 64, ((tpage >> 4) & 1) * 256);
        (tx == self.x && ty >= self.y && ty < self.y + self.h).then(|| ty - self.y)
    }
}

fn material(p: &Prim, page: &Page) -> String {
    let mut m = if p.textured() {
        match page.row_of(p.tpage) {
            Some(_) => {
                let mut m = format!("c{:04x}", p.clut);
                let abr = (p.tpage >> 5) & 3;
                if abr != 0 {
                    let _ = write!(m, "_a{abr}");
                }
                m
            }
            None => format!("t{:04x}_c{:04x}", p.tpage, p.clut),
        }
    } else {
        "flat".to_string()
    };
    if p.semi() {
        m.push_str("_st");
    }
    if p.raw() {
        m.push_str("_raw");
    }
    let n = p.n as usize;
    if p.gouraud() && p.rgb[..n].iter().all(|c| *c == p.rgb[0]) {
        m.push_str("_g");
    }
    m
}

/// Exports `c` (chunk `id`).
pub fn export(c: &Chunk, id: u32) -> Exported {
    let page = Page::of(&c.texture);
    let mut verts: Vec<String> = Vec::new();
    let mut vkeys: HashMap<([i16; 3], [u8; 3]), usize> = HashMap::new();
    let mut vkey = |verts: &mut Vec<String>, pos: [i16; 3], rgb: [u8; 3]| -> usize {
        *vkeys.entry((pos, rgb)).or_insert_with(|| {
            let f = |c: u8| c as f64 / 255.0;
            verts.push(format!("v {} {} {} {:.4} {:.4} {:.4}", pos[0], -(pos[1] as i32), pos[2], f(rgb[0]), f(rgb[1]), f(rgb[2])));
            verts.len()
        })
    };
    // The pool first, in order, with each vertex's first colour.
    let mut first: Vec<Option<[u8; 3]>> = vec![None; c.pool.len()];
    for g in &c.groups {
        for p in prims_in_list_order(g) {
            for k in 0..p.n as usize {
                first[p.idx[k] as usize].get_or_insert(p.rgb[if p.gouraud() { k } else { 0 }]);
            }
        }
    }
    for (i, pos) in c.pool.iter().enumerate() {
        vkey(&mut verts, *pos, first[i].unwrap_or([128, 128, 128]));
    }
    let mut vts: Vec<String> = Vec::new();
    let mut vtkeys: HashMap<(u16, u16, u16), usize> = HashMap::new();
    let mut faces = String::new();
    let mut cluts: BTreeMap<u16, ()> = BTreeMap::new();
    let mut materials: Vec<String> = Vec::new();
    for (gi, g) in c.groups.iter().enumerate() {
        let _ = writeln!(faces, "g group{gi:02}");
        let mut current = String::new();
        for p in prims_in_list_order(g) {
            let m = material(p, &page);
            if m != current {
                let _ = writeln!(faces, "usemtl {m}");
                if !materials.contains(&m) {
                    materials.push(m.clone());
                }
                current = m;
            }
            let n = p.n as usize;
            let row = if p.textured() { page.row_of(p.tpage) } else { None };
            if row.is_some() {
                cluts.insert(p.clut, ());
            }
            let order: &[usize] = if n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
            faces.push('f');
            for &k in order {
                let rgb = p.rgb[if p.gouraud() { k } else { 0 }];
                let vi = vkey(&mut verts, c.pool[p.idx[k] as usize], rgb);
                if p.textured() {
                    let (u, y, h) = match row {
                        Some(r) => (p.uv[k][0] as u16, p.uv[k][1] as u16 + r, page.h),
                        None => (p.uv[k][0] as u16, p.uv[k][1] as u16, 256),
                    };
                    let ti = *vtkeys.entry((u, y, h)).or_insert_with(|| {
                        vts.push(format!("vt {:.6} {:.6}", (u as f64 + 0.5) / 256.0, 1.0 - (y as f64 + 0.5) / h as f64));
                        vts.len()
                    });
                    let _ = write!(faces, " {vi}/{ti}");
                } else {
                    let _ = write!(faces, " {vi}");
                }
            }
            faces.push('\n');
        }
    }
    let mut obj = format!(
        "# Guardian's Crusade chunk {id} -- edit, then import it in Living Tool's world editor\n\
         # units: chunk-local, Y up (negated from PSX), 32 units = one walk cell\nmtllib map.mtl\n"
    );
    for v in &verts {
        obj.push_str(v);
        obj.push('\n');
    }
    for v in &vts {
        obj.push_str(v);
        obj.push('\n');
    }
    obj.push_str(&faces);

    let mut mtl = String::new();
    let mut images = Vec::new();
    for clut in cluts.keys() {
        if let Some(png) = page_png(&c.texture, *clut) {
            images.push((format!("c{clut:04x}.png"), png));
        }
    }
    for m in &materials {
        let _ = writeln!(mtl, "newmtl {m}\nKd 1 1 1");
        if let Some(clut) = m.strip_prefix('c').and_then(|r| u16::from_str_radix(&r[..4], 16).ok()) {
            if images.iter().any(|(n, _)| *n == format!("c{clut:04x}.png")) {
                let _ = writeln!(mtl, "map_Kd c{clut:04x}.png");
            }
        } else if let Some(t) = m.strip_prefix('t') {
            let _ = writeln!(mtl, "# texture page 0x{} is shared, not in this chunk", &t[..4]);
        }
    }
    let meta = Meta {
        chunk: id,
        groups: c
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| MetaGroup { name: format!("group{i:02}"), center: g.centre, bbox: bbox(&c.pool, g) })
            .collect(),
        notes: "Group centres are culling spheres. A group moved or reshaped since the export (its bbox differs) or missing here gets the middle of its bounding box on import.".into(),
    };
    Exported { obj, mtl, meta: serde_json::to_string_pretty(&meta).unwrap_or_default(), images }
}

/// Bounding box of the vertices a group uses.
fn bbox(pool: &[[i16; 3]], g: &Group) -> Option<[i16; 6]> {
    let mut b: Option<[i16; 6]> = None;
    for p in &g.prims {
        for &i in &p.idx[..p.n as usize] {
            let v = *pool.get(i as usize)?;
            let x = b.get_or_insert([v[0], v[1], v[2], v[0], v[1], v[2]]);
            for a in 0..3 {
                x[a] = x[a].min(v[a]);
                x[a + 3] = x[a + 3].max(v[a]);
            }
        }
    }
    b
}

/// The group's prims in the order serialize writes them (list by list).
fn prims_in_list_order(g: &Group) -> impl Iterator<Item = &Prim> {
    (0..8u8).flat_map(move |l| g.prims.iter().filter(move |p| p.list == l))
}

/// The own page through CLUT `clut` as an RGBA PNG (colour 0x0000 transparent).
fn page_png(t: &Texture, clut: u16) -> Option<Vec<u8>> {
    let (rect, entries) = t.clut.as_ref()?;
    let row = ((clut >> 6) as i32) - rect.y as i32;
    let col = ((clut & 0x3F) as i32) * 16 - rect.x as i32;
    if row < 0 || col < 0 || row >= rect.h as i32 || col + 16 > rect.w as i32 {
        return None;
    }
    let pal: Vec<[u8; 4]> = (0..16).map(|i| bgr555_to_rgba(entries[row as usize * rect.w as usize + col as usize + i])).collect();
    let (w, h) = (t.image.w as usize * 4, t.image.h as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let b = t.pixels[y * (w / 2) + x / 2];
            rgba.extend_from_slice(&pal[if x & 1 == 0 { b & 15 } else { b >> 4 } as usize]);
        }
    }
    encode_png(w as u32, h as u32, &rgba)
}

/// RGBA8 → PNG.
pub(crate) fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/* ───────────────────────── Import ───────────────────────── */

struct Face {
    group: usize,
    material: String,
    line: usize,
    corners: Vec<(usize, Option<usize>)>,
}

fn err(code: &'static str, line: usize, msg: impl std::fmt::Display) -> EditError {
    EditError::new(code, if line > 0 { format!("map.obj line {line}: {msg}") } else { msg.to_string() })
}

/// Parsed material: own page or tpage word, CLUT, blend bits, code flags, keep-gouraud.
struct Mat {
    textured: bool,
    tpage: Option<u16>,
    clut: u16,
    abr: u16,
    flags: u8,
    keep_g: bool,
}

fn parse_material(name: &str, line: usize) -> Result<Mat> {
    let bad = || err("CHUNK_OBJ_MATERIAL", line, format!("unknown material “{name}” (use cXXXX, tYYYY_cXXXX or flat, with _aN, _st, _raw, _g)"));
    let hex = |s: &str, p: char| s.strip_prefix(p).filter(|r| r.len() == 4).and_then(|r| u16::from_str_radix(r, 16).ok());
    let mut toks = name.split('_');
    let head = toks.next().ok_or_else(bad)?;
    let mut m = Mat { textured: head != "flat", tpage: None, clut: 0, abr: 0, flags: 0, keep_g: false };
    if head.starts_with('c') {
        m.clut = hex(head, 'c').ok_or_else(bad)?;
    } else if head.starts_with('t') {
        m.tpage = Some(hex(head, 't').ok_or_else(bad)?);
        m.clut = hex(toks.next().ok_or_else(bad)?, 'c').ok_or_else(bad)?;
    } else if head != "flat" {
        return Err(bad());
    }
    for t in toks {
        match t {
            "st" => m.flags |= 2,
            "raw" => m.flags |= 1,
            "g" => m.keep_g = true,
            "a1" | "a2" | "a3" if m.textured && m.tpage.is_none() => m.abr = (t.as_bytes()[1] - b'0') as u16,
            _ => return Err(bad()),
        }
    }
    Ok(m)
}

/// Rebuilds `base`'s geometry from an OBJ (and the export's `meta.json`).
pub fn import(base: &Chunk, obj: &str, meta: Option<&str>) -> Result<(Chunk, Imported)> {
    let meta: Option<Meta> = match meta {
        Some(m) => Some(serde_json::from_str(m).map_err(|e| err("CHUNK_OBJ_META", 0, format!("meta.json doesn't parse: {e}")))?),
        None => None,
    };
    let mut verts: Vec<([f64; 3], [f64; 3])> = Vec::new();
    let mut vts: Vec<[f64; 2]> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    let mut faces: Vec<Face> = Vec::new();
    let (mut group, mut material) = (None::<String>, "flat".to_string());
    for (ln, raw) in obj.lines().enumerate() {
        let line = ln + 1;
        let mut t = raw.split_whitespace();
        let Some(tag) = t.next() else { continue };
        let nums = |t: std::str::SplitWhitespace| -> Result<Vec<f64>> {
            t.map(|s| s.parse::<f64>().map_err(|_| err("CHUNK_OBJ_SYNTAX", line, format!("“{s}” is not a number")))).collect()
        };
        match tag {
            "v" => {
                let n = nums(t)?;
                if n.len() < 3 {
                    return Err(err("CHUNK_OBJ_SYNTAX", line, "a vertex needs x y z"));
                }
                let rgb = if n.len() >= 6 { [n[3], n[4], n[5]] } else { [128.0 / 255.0; 3] };
                verts.push(([n[0], n[1], n[2]], rgb));
            }
            "vt" => {
                let n = nums(t)?;
                if n.len() < 2 {
                    return Err(err("CHUNK_OBJ_SYNTAX", line, "a texture coordinate needs u v"));
                }
                vts.push([n[0], n[1]]);
            }
            "g" | "o" => group = t.next().map(str::to_string).or(group),
            "usemtl" => material = t.next().unwrap_or("flat").to_string(),
            "f" => {
                let name = group.clone().unwrap_or_else(|| "group00".into());
                let gi = match groups.iter().position(|g| *g == name) {
                    Some(i) => i,
                    None => {
                        groups.push(name);
                        groups.len() - 1
                    }
                };
                let mut corners = Vec::new();
                for c in t {
                    let mut parts = c.split('/');
                    let index = |s: Option<&str>, len: usize, what: &str| -> Result<Option<usize>> {
                        let Some(s) = s.filter(|s| !s.is_empty()) else { return Ok(None) };
                        let i: i64 = s.parse().map_err(|_| err("CHUNK_OBJ_SYNTAX", line, format!("bad {what} index “{s}”")))?;
                        let k = if i > 0 { i - 1 } else { len as i64 + i };
                        if k < 0 || k as usize >= len {
                            return Err(err("CHUNK_OBJ_INDEX", line, format!("{what} {i} doesn't exist")));
                        }
                        Ok(Some(k as usize))
                    };
                    let vi = index(parts.next(), verts.len(), "vertex")?.ok_or_else(|| err("CHUNK_OBJ_SYNTAX", line, "a face corner needs a vertex"))?;
                    let ti = index(parts.next(), vts.len(), "texture coordinate")?;
                    corners.push((vi, ti));
                }
                if !(3..=4).contains(&corners.len()) {
                    return Err(err("CHUNK_OBJ_NGON", line, format!("a face has {} corners; triangulate or use quads", corners.len())));
                }
                faces.push(Face { group: gi, material: material.clone(), line, corners });
            }
            _ => {}
        }
    }
    if faces.is_empty() {
        return Err(err("CHUNK_OBJ_EMPTY", 0, "map.obj has no faces"));
    }

    let page = Page::of(&base.texture);
    let to_i16 = |f: f64, line: usize| -> Result<i16> {
        let r = f.round();
        if !(i16::MIN as f64..=i16::MAX as f64).contains(&r) {
            return Err(err("CHUNK_OBJ_RANGE", line, format!("coordinate {f} is outside −32768…32767")));
        }
        Ok(r as i16)
    };
    let to_u8 = |f: f64| (f * 255.0).round().clamp(0.0, 255.0) as u8;
    // Pool: referenced positions in OBJ vertex order.
    let mut used = vec![false; verts.len()];
    for f in &faces {
        for &(vi, _) in &f.corners {
            used[vi] = true;
        }
    }
    let mut pool: Vec<[i16; 3]> = Vec::new();
    let mut slot: HashMap<[i16; 3], u16> = HashMap::new();
    let mut vmap = vec![0u16; verts.len()];
    for (i, (p, _)) in verts.iter().enumerate() {
        if !used[i] {
            continue;
        }
        let pos = [to_i16(p[0], 0)?, to_i16(-p[1], 0)?, to_i16(p[2], 0)?];
        let n = pool.len();
        let k = *slot.entry(pos).or_insert_with(|| {
            pool.push(pos);
            n as u16
        });
        if pool.len() > super::chunk::MAX_VERTICES {
            return Err(err("CHUNK_VERTICES", 0, format!("more than {} distinct vertex positions", super::chunk::MAX_VERTICES)));
        }
        vmap[i] = k;
    }

    let mut out_groups: Vec<Group> = groups.iter().map(|_| Group { centre: [0; 3], prims: Vec::new() }).collect();
    let mut mats: HashMap<String, Mat> = HashMap::new();
    for f in &faces {
        if !mats.contains_key(&f.material) {
            mats.insert(f.material.clone(), parse_material(&f.material, f.line)?);
        }
        let m = &mats[&f.material];
        let n = f.corners.len();
        let loop_order: &[usize] = if n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
        let corners: Vec<(usize, Option<usize>)> = loop_order.iter().map(|&k| f.corners[k]).collect();
        let cols: Vec<[u8; 3]> = corners.iter().map(|&(vi, _)| verts[vi].1.map(to_u8)).collect();
        let gouraud = m.keep_g || cols.iter().any(|c| *c != cols[0]);
        let list = (if gouraud { 4 } else { 0 }) + (if n == 4 { 2 } else { 0 }) + (if m.textured { 1 } else { 0 });
        let mut p = Prim { list, code: 0x20 | list << 2 | m.flags, n: n as u8, idx: [0; 4], rgb: [[0; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        for (k, &(vi, _)) in corners.iter().enumerate() {
            p.idx[k] = vmap[vi];
            p.rgb[k] = if gouraud { cols[k] } else { cols[0] };
        }
        if m.textured {
            p.clut = m.clut;
            let (h, own) = match m.tpage {
                Some(_) => (256.0, false),
                None => (page.h as f64, true),
            };
            let mut rows = Vec::new();
            for (k, &(_, ti)) in corners.iter().enumerate() {
                let ti = ti.ok_or_else(|| err("CHUNK_OBJ_NO_UV", f.line, format!("a face with material {} has no texture coordinates", f.material)))?;
                let [vu, vv] = vts[ti];
                let u = (vu * 256.0 - 0.5).round().clamp(0.0, 255.0) as u16;
                let y = ((1.0 - vv) * h - 0.5).round().clamp(0.0, h - 1.0) as u16;
                let y = if own { y + page.y } else { y };
                rows.push(y >> 8);
                p.uv[k] = [u as u8, (y & 0xFF) as u8];
            }
            p.tpage = match m.tpage {
                Some(t) => t,
                None => {
                    if rows.iter().any(|&r| r != rows[0]) {
                        return Err(err("CHUNK_OBJ_PAGE_ROW", f.line, "a face spans rows 255 and 256 of the texture page; split it there"));
                    }
                    page.x / 64 | rows[0] << 4 | m.abr << 5
                }
            };
        }
        out_groups[f.group].prims.push(p);
    }

    let mut chunk = base.clone();
    chunk.pool = pool;
    chunk.groups = out_groups;
    let kept: HashMap<&str, &MetaGroup> = meta.iter().flat_map(|m| m.groups.iter().map(|g| (g.name.as_str(), g))).collect();
    let mut recentred = 0;
    for (i, name) in groups.iter().enumerate() {
        match kept.get(name.as_str()) {
            Some(m) if m.bbox.is_none() || m.bbox == bbox(&chunk.pool, &chunk.groups[i]) => chunk.groups[i].centre = m.center,
            _ => {
                chunk.recompute_centre(i);
                recentred += 1;
            }
        }
    }
    let sizes = chunk.sizes()?;
    let report = Imported {
        groups: chunk.groups.len() as u32,
        polygons: chunk.groups.iter().map(|g| g.prims.len() as u32).sum(),
        vertices: chunk.pool.len() as u32,
        recentred,
        sizes,
    };
    Ok((chunk, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri_chunk() -> Chunk {
        let mut c = Chunk::blank(2);
        c.pool = vec![[0, -128, 0], [64, -128, 0], [0, -128, 64], [64, -128, 64]];
        let ft4 = Prim { list: 3, code: 0x2E, n: 4, idx: [0, 1, 2, 3], rgb: [[128; 3]; 4], uv: [[0, 250], [16, 250], [0, 255], [16, 255]], clut: 0x7842, tpage: 0x0C | 0x40 };
        let g3 = Prim { list: 4, code: 0x30, n: 3, idx: [0, 1, 2, 0], rgb: [[10, 20, 30], [40, 50, 60], [70, 80, 90], [0; 3]], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        c.groups.push(Group { centre: [1, 2, 3], prims: vec![ft4, g3] });
        c
    }

    #[test]
    fn export_then_import_is_identity() {
        let c = tri_chunk();
        let e = export(&c, 330);
        assert!(e.obj.contains("usemtl c7842_a2_st\n"), "{}", e.obj);
        let (d, r) = import(&c, &e.obj, Some(&e.meta)).unwrap();
        assert_eq!(r.recentred, 0);
        assert_eq!(d.serialize().unwrap(), c.serialize().unwrap());
        // Without meta.json the centre comes from the bounding box.
        let (d, r) = import(&c, &e.obj, None).unwrap();
        assert_eq!((r.recentred, d.groups[0].centre), (1, [32, -128, 32]));
        // A group moved after the export (z + 64 throughout) gets a new centre; the kept one is stale.
        let moved: String = e
            .obj
            .lines()
            .map(|l| match l.strip_prefix("v ") {
                Some(rest) => {
                    let mut t: Vec<String> = rest.split(' ').map(str::to_string).collect();
                    t[2] = (t[2].parse::<i32>().unwrap() + 64).to_string();
                    format!("v {}\n", t.join(" "))
                }
                None => format!("{l}\n"),
            })
            .collect();
        let (d, r) = import(&c, &moved, Some(&e.meta)).unwrap();
        assert_eq!((r.recentred, d.groups[0].centre), (1, [32, -128, 96]));
    }

    #[test]
    fn bad_input_is_refused_with_a_line() {
        let c = tri_chunk();
        let code = |obj: &str| import(&c, obj, None).unwrap_err().code;
        assert_eq!(code("v 0 0 0\nv 1 0 0\nv 0 0 1\nv 1 0 1\nv 2 0 2\nf 1 2 3 4 5\n"), "CHUNK_OBJ_NGON");
        assert_eq!(code("v 0 0 0\nv 1 0 0\nv 0 0 1\nusemtl c7842\nf 1 2 3\n"), "CHUNK_OBJ_NO_UV");
        assert_eq!(code("v 0 0 0\nv 1 0 0\nv 0 0 1\nusemtl brick\nf 1 2 3\n"), "CHUNK_OBJ_MATERIAL");
        assert_eq!(code("v 0 0 0\nf 1 2 3\n"), "CHUNK_OBJ_INDEX");
        assert_eq!(code("v 40000 0 0\nv 1 0 0\nv 0 0 1\nf 1 2 3\n"), "CHUNK_OBJ_RANGE");
        assert_eq!(code("# nothing\n"), "CHUNK_OBJ_EMPTY");
        // Page rows 255 and 256 in one face (a 512-row page).
        let mut big = c.clone();
        big.texture.image.h = 512;
        big.texture.pixels = vec![0; 64 * 512 * 2];
        let obj = "v 0 0 0\nv 1 0 0\nv 0 0 1\nvt 0.1 0.5005\nvt 0.2 0.4995\nvt 0.3 0.4995\nusemtl c7842\nf 1/1 2/2 3/3\n";
        assert_eq!(import(&big, obj, None).unwrap_err().code, "CHUNK_OBJ_PAGE_ROW");
    }

    #[test]
    fn blender_style_faces_and_missing_colours_import() {
        let c = tri_chunk();
        let obj = "o Cube\nv 0 128 0\nv 64 128 0\nv 0 128 64\nvn 0 1 0\ns off\nusemtl flat\nf 1//1 2//1 3//1\n";
        let (d, r) = import(&c, obj, None).unwrap();
        assert_eq!((r.groups, r.polygons, r.vertices), (1, 1, 3));
        let p = d.groups[0].prims[0];
        assert_eq!((p.list, p.rgb[0], d.pool[0]), (0, [128, 128, 128], [0, -128, 0]));
    }
}
