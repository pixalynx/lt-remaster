//! Character models in and out (screen 06 §5, research 06 phase 1): a model
//! exported to OBJ, edited in Blender (or any glTF/OBJ made from scratch), and
//! imported back into its container as a new TMD.
//!
//! **Export** writes one object per TMD object as OBJ + MTL: Y up (the
//! game's Y negated), model units, faces in TMD index order (counter-
//! clockwise front, measured on every retail toy), quads as 4-corner faces
//! in loop order (the TMD's 0-1-3-2 is restored on import). Materials name
//! the binding: `t%03x_c%04x` (tpage word, CLUT id), plus `_st`
//! (semi-transparent), `_lit` (GTE-lit: normals kept) and `_g` (per-vertex
//! colour). UVs of the model's own texture are relative to its TIM, so the
//! PNG the texture viewer exports maps straight onto the mesh. Models that
//! ship no TIM (NPCs) get UVs relative to each whole page and one PNG per
//! page, decoded from the VRAM they are drawn with ([`export_paged`]).
//!
//! **Import** turns any mesh ([`super::meshin::InMesh`]) into one TMD object:
//!
//! | textured | normals | colours | → tri / quad |
//! |---|---|---|---|
//! | yes | flat / smooth | – | 2400/2C00 · 3400/3C00 (lit) |
//! | yes | none (prelit) | one / per vertex | 2501/2D01 · 3501/3D01 |
//! | no | flat / smooth | one / per vertex | 2000/2800 · 2004/2804 · 3000/3800 · 3004/3804 |
//! | no | none (prelit) | one / per vertex | 2101/2901 · 3101/3901 |
//!
//! Semi-transparent materials set the ABE bit (`mode | 0x02`). Field
//! targets (NPCs) are always prelit: the field has no light rig for models.
//! Vertices are welded by position; normals are quantised to 4096 and
//! pooled. Colours: 0–1 → `round(c·255)`, where 128 is 1.0 modulation on a
//! textured polygon; no colour = 128.
//!
//! **Poses** are vertex morphs over the base mesh's vertex order. An import
//! that keeps the vertex count keeps them (topology-locked: move vertices,
//! recolour, retexture). Otherwise every pose is rewritten as one record
//! that moves nothing, so the model holds its rest shape in every animation until poses
//! are authored for it (phase 1, animation).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::container::le32;

use super::meshin::InMesh;
use super::tmdcodec::{class_words, Object, Prim, Sizes, Tmd};
use super::{EditError, Result};

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/// Where a model's own texture sits: the binding most of its polygons use
/// and the TIM's rectangle inside that page, in texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TexBind {
    pub tsb: u16,
    pub cba: u16,
    pub u0: u16,
    pub v0: u16,
    pub w: u16,
    pub h: u16,
}

impl TexBind {
    /// A whole page (no own TIM known).
    pub fn page(tsb: u16, cba: u16) -> Self {
        Self { tsb, cba, u0: 0, v0: 0, w: 256, h: 256 }
    }

    /// The binding of a model whose own TIM is `tim` (TIM bytes), given the
    /// dominant (tsb, cba) of its polygons.
    pub fn of_tim(tsb: u16, cba: u16, tim: &[u8]) -> Self {
        let Some(t) = crate::tim::Tim::parse(tim) else { return Self::page(tsb, cba) };
        let bpp = match (tsb >> 7) & 3 {
            0 => 4u16,
            1 => 8,
            _ => 16,
        };
        let (px, py) = ((tsb & 0xF) * 64, ((tsb >> 4) & 1) * 256);
        let i = t.info.image;
        let per = 16 / bpp;
        // A page is 256 texels wide: 64 halfwords at 4 bpp, 128 at 8, 256 at 16.
        if i.x < px || i.y < py || i.x >= px + 256 / per || i.y >= py + 256 {
            return Self::page(tsb, cba);
        }
        let u0 = (i.x - px) * per;
        let v0 = i.y - py;
        let w = (i.w * per).min(256 - u0.min(255));
        let h = i.h.min(256 - v0.min(255));
        Self { tsb, cba, u0, v0, w, h }
    }

    /// Texel of a 0–1 UV over the TIM (V up). UVs past 0–1 reach the rest
    /// of the page (some retail polygons sample just outside their TIM);
    /// `None` when they fall off the page.
    fn texel(self, uv: [f32; 2]) -> Option<(u8, u8)> {
        let axis = |t: f32, size: u16| {
            let f = (t * size as f32).floor();
            if (0.0..=1.0).contains(&t) {
                f.min(size as f32 - 1.0)
            } else {
                f
            }
        };
        let u = self.u0 as f32 + axis(uv[0], self.w);
        let v = self.v0 as f32 + axis(1.0 - uv[1], self.h);
        let on = (0.0..=255.0).contains(&u) && (0.0..=255.0).contains(&v);
        on.then_some((u as u8, v as u8))
    }

    fn uv(self, u: u8, v: u8) -> [f32; 2] {
        [(u as f32 - self.u0 as f32 + 0.5) / self.w as f32, 1.0 - (v as f32 - self.v0 as f32 + 0.5) / self.h as f32]
    }
}

/// The (tsb, cba) most textured polygons of `tmd` use.
pub fn dominant_binding(tmd: &Tmd) -> Option<(u16, u16)> {
    let mut count: HashMap<(u16, u16), usize> = HashMap::new();
    for o in &tmd.objects {
        for p in &o.prims {
            if p.mode & 0x04 != 0 && p.words.len() >= 2 {
                *count.entry(((p.words[1] >> 16) as u16, (p.words[0] >> 16) as u16)).or_default() += 1;
            }
        }
    }
    count.into_iter().max_by_key(|&(k, n)| (n, std::cmp::Reverse(k))).map(|(k, _)| k)
}

/* ───────────────────────── Decoding one primitive ───────────────────────── */

struct Decoded {
    n: usize,
    verts: [usize; 4],
    normals: Option<[usize; 4]>,
    uvs: Option<[(u8, u8); 4]>,
    tsb: u16,
    cba: u16,
    colours: [[u8; 3]; 4],
    /// Colour words stored: 0 (lit textured), 1 (one for the face) or n.
    ncol: usize,
    grd: bool,
}

fn decode(p: &Prim) -> Option<Decoded> {
    let n = if p.mode & 0x08 != 0 { 4 } else { 3 };
    let textured = p.mode & 0x04 != 0;
    let iip = p.mode & 0x10 != 0;
    let lit = p.flag & 0x01 == 0;
    let grd_flag = p.flag & 0x04 != 0;
    let w = |i: usize| p.words.get(i).copied();
    let mut k = 0;
    let mut d = Decoded { n, verts: [0; 4], normals: None, uvs: None, tsb: 0, cba: 0, colours: [[128; 3]; 4], ncol: 0, grd: false };
    if textured {
        let mut uv = [(0u8, 0u8); 4];
        for (c, slot) in uv.iter_mut().enumerate().take(n) {
            let x = w(k + c)?;
            *slot = (x as u8, (x >> 8) as u8);
        }
        d.cba = (w(k)? >> 16) as u16;
        d.tsb = (w(k + 1)? >> 16) as u16;
        d.uvs = Some(uv);
        k += n;
    }
    let colours = match (lit, textured) {
        (true, true) => 0,
        (true, false) => {
            if grd_flag {
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
    for c in 0..colours {
        let x = w(k + c)?;
        d.colours[c] = [x as u8, (x >> 8) as u8, (x >> 16) as u8];
    }
    if colours == 1 {
        d.colours = [d.colours[0]; 4];
    }
    d.ncol = colours;
    d.grd = colours > 1;
    k += colours;
    let lo = |x: u32| (x & 0xFFFF) as usize;
    let hi = |x: u32| (x >> 16) as usize;
    if lit {
        if iip {
            let mut nr = [0; 4];
            for (c, (slot, vert)) in nr.iter_mut().zip(d.verts.iter_mut()).enumerate().take(n) {
                let x = w(k + c)?;
                *slot = lo(x);
                *vert = hi(x);
            }
            d.normals = Some(nr);
        } else {
            let a = w(k)?;
            let b = w(k + 1)?;
            d.normals = Some([lo(a); 4]);
            d.verts[0] = hi(a);
            d.verts[1] = lo(b);
            d.verts[2] = hi(b);
            if n == 4 {
                d.verts[3] = lo(w(k + 2)?);
            }
        }
    } else {
        let a = w(k)?;
        let b = w(k + 1)?;
        d.verts = [lo(a), hi(a), lo(b), if n == 4 { hi(b) } else { 0 }];
    }
    Some(d)
}

/// How two models differ as the game draws them, primitive by primitive
/// (normals by value, colour-word padding ignored).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Differences {
    pub vertices: u32,
    pub primitives: u32,
    pub classes: u32,
    pub corners: u32,
    pub uvs: u32,
    pub bindings: u32,
    pub colours: u32,
    pub normals: u32,
}

impl Differences {
    pub fn none(&self) -> bool {
        *self == Self::default()
    }
}

pub fn differences(a: &Object, b: &Object) -> Differences {
    let mut d = Differences { vertices: (a.verts != b.verts) as u32, primitives: a.prims.len().abs_diff(b.prims.len()) as u32, ..Default::default() };
    for (p, q) in a.prims.iter().zip(&b.prims) {
        if (p.mode, p.flag) != (q.mode, q.flag) {
            d.classes += 1;
            continue;
        }
        let (Some(x), Some(y)) = (decode(p), decode(q)) else { continue };
        d.corners += (x.verts[..x.n] != y.verts[..y.n]) as u32;
        d.uvs += (x.uvs.map(|u| u[..x.n].to_vec()) != y.uvs.map(|u| u[..y.n].to_vec())) as u32;
        d.bindings += ((x.tsb, x.cba) != (y.tsb, y.cba)) as u32;
        d.colours += (x.colours[..x.ncol.min(4)] != y.colours[..y.ncol.min(4)]) as u32;
        let nv = |o: &Object, r: Option<[usize; 4]>, n: usize| r.map(|r| r[..n].iter().map(|&i| o.normals.get(i).map(|v| [v[0], v[1], v[2]])).collect::<Vec<_>>());
        d.normals += (nv(a, x.normals, x.n) != nv(b, y.normals, y.n)) as u32;
    }
    d
}

/* ───────────────────────── Export ───────────────────────── */

/// The colour each vertex is exported with: the first colour a gouraud
/// face draws it with (one-colour faces carry theirs in the material
/// name). `None` when the model has no gouraud faces.
fn vertex_colours(o: &Object, decoded: &[Option<Decoded>]) -> Option<Vec<[u8; 3]>> {
    if !decoded.iter().flatten().any(|d| d.grd) {
        return None;
    }
    let mut vcol = vec![[128u8; 3]; o.verts.len()];
    let mut set = vec![false; o.verts.len()];
    for d in decoded.iter().flatten().filter(|d| d.grd) {
        for c in 0..d.n {
            if let Some(k) = set.get_mut(d.verts[c]).filter(|k| !**k) {
                *k = true;
                vcol[d.verts[c]] = d.colours[c];
            }
        }
    }
    Some(vcol)
}

/// An OBJ + MTL of `tmd`; `own` is the model's own texture binding (UVs of
/// polygons on it are relative to its TIM; others to their whole page).
pub fn export_obj(tmd: &Tmd, own: Option<TexBind>, name: &str) -> (String, String) {
    let maps: Vec<((u16, u16), String)> = own.map(|b| ((b.tsb, b.cba), format!("{name}.png"))).into_iter().collect();
    let (obj, mtl, _) = write_obj(tmd, own, &maps, name);
    (obj, mtl)
}

/// What [`write_obj`] couldn't write as stored; each polygon leaves a
/// `# left out:` or `# no normals:` comment line.
#[derive(Debug, Clone, Copy, Default)]
struct Omitted {
    /// Polygons that don't decode or index past the vertex pool (the 3D
    /// view drops them too).
    polygons: u32,
    /// Lit polygons whose normals lie past the end of the TMD (`npc/12#0`
    /// declares 222 there), written without normals.
    normals: u32,
}

/// [`export_obj`] with the MTL's `map_Kd` per binding: materials on
/// (tsb, cba) name that file.
fn write_obj(tmd: &Tmd, own: Option<TexBind>, maps: &[((u16, u16), String)], name: &str) -> (String, String, Omitted) {
    let mut obj = String::new();
    let mut mtl = String::new();
    let _ = writeln!(obj, "# Living Tool model export: {name}\n# units: model, Y up (negated from PSX); faces counter-clockwise; quads in loop order\nmtllib {name}.mtl");
    let mut mats: BTreeMap<String, ()> = BTreeMap::new();
    let (mut vbase, mut tbase, mut nbase) = (1usize, 1usize, 1usize);
    let nv: usize = tmd.objects.iter().map(|o| o.verts.len()).sum();
    let nn: usize = tmd.objects.iter().map(|o| o.normals.len()).sum();
    let mut omitted = Omitted::default();
    for (oi, o) in tmd.objects.iter().enumerate() {
        let _ = writeln!(obj, "o object{oi}");
        let decoded: Vec<Option<Decoded>> = o.prims.iter().map(decode).collect();
        let vcol = vertex_colours(o, &decoded);
        for (i, v) in o.verts.iter().enumerate() {
            match vcol.as_ref().map(|c| c[i]) {
                Some(c) => {
                    let _ = writeln!(obj, "v {} {} {} {:.4} {:.4} {:.4}", v[0], -(v[1] as i32), v[2], c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
                }
                None => {
                    let _ = writeln!(obj, "v {} {} {}", v[0], -(v[1] as i32), v[2]);
                }
            }
        }
        for nrm in &o.normals {
            let _ = writeln!(obj, "vn {:.5} {:.5} {:.5}", nrm[0] as f32 / 4096.0, -(nrm[1] as f32) / 4096.0, nrm[2] as f32 / 4096.0);
        }
        let mut current = String::new();
        let past = |d: &Decoded| d.verts[..d.n].iter().any(|&v| vbase - 1 + v >= nv);
        for (pi, (p, d)) in o.prims.iter().zip(&decoded).enumerate() {
            let Some(d) = d.as_ref().filter(|d| !past(d)) else {
                let why = if d.is_some() { "indexes past the vertex pool" } else { "doesn't decode" };
                let _ = writeln!(obj, "# left out: polygon {pi} of object{oi} {why}");
                omitted.polygons += 1;
                continue;
            };
            let normals = d.normals.filter(|nr| nr[..d.n].iter().all(|&k| nbase - 1 + k < nn));
            if normals.is_none() && d.normals.is_some() {
                let _ = writeln!(obj, "# no normals: polygon {pi} of object{oi} indexes normals past the end of the TMD");
                omitted.normals += 1;
            }
            let lit = p.flag & 0x01 == 0;
            let mut mname = if p.mode & 0x04 != 0 { format!("t{:03x}_c{:04x}", d.tsb, d.cba) } else { "flat".to_string() };
            if p.mode & 0x02 != 0 {
                mname.push_str("_st");
            }
            if lit {
                mname.push_str("_lit");
                if p.mode & 0x10 != 0 {
                    mname.push_str("_sm");
                }
            }
            if d.grd {
                mname.push_str("_g");
            } else if d.ncol == 1 {
                let c = d.colours[0];
                let _ = write!(mname, "_k{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
            }
            if mname != current {
                let _ = writeln!(obj, "usemtl {mname}");
                current = mname.clone();
                mats.insert(mname, ());
            }
            let order: &[usize] = if d.n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
            let mut face = String::from("f");
            for &c in order {
                let v = vbase + d.verts[c];
                let t = d.uvs.map(|uv| {
                    let bind = own.filter(|b| b.tsb == d.tsb && b.cba == d.cba).unwrap_or(TexBind::page(d.tsb, d.cba));
                    let f = bind.uv(uv[c].0, uv[c].1);
                    let _ = writeln!(obj, "vt {:.5} {:.5}", f[0], f[1]);
                    tbase += 1;
                    tbase - 1
                });
                let n = normals.map(|nr| nbase + nr[c]);
                let _ = match (t, n) {
                    (Some(t), Some(n)) => write!(face, " {v}/{t}/{n}"),
                    (Some(t), None) => write!(face, " {v}/{t}"),
                    (None, Some(n)) => write!(face, " {v}//{n}"),
                    (None, None) => write!(face, " {v}"),
                };
            }
            let _ = writeln!(obj, "{face}");
        }
        vbase += o.verts.len();
        nbase += o.normals.len();
    }
    let _ = writeln!(mtl, "# Living Tool: tXXX_cYYYY = texture page word and CLUT id; _st semi-transparent, _lit GTE-lit,\n# _g per-vertex colour, _kRRGGBB one colour for the face (raw bytes; 80 = 1.0 on a texture)");
    for m in mats.keys() {
        let mat = super::meshin::Material::named(m);
        let kd = match mat.color {
            // Shown as the game draws it: textures are modulated around 0x80, flat colours are absolute.
            Some(c) => {
                let div = if mat.textured { 128.0 } else { 255.0 };
                c.map(|x| (x as f32 / div).min(1.0))
            }
            None => [1.0; 3],
        };
        let _ = writeln!(mtl, "newmtl {m}\nKd {:.4} {:.4} {:.4}", kd[0], kd[1], kd[2]);
        if let Some((_, file)) = maps.iter().find(|((tsb, cba), _)| m.starts_with(&format!("t{tsb:03x}_c{cba:04x}"))) {
            let _ = writeln!(mtl, "map_Kd {file}");
        }
        let _ = writeln!(mtl);
    }
    (obj, mtl, omitted)
}

/* ───────────────────────── Import ───────────────────────── */

/// How the importer lights the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Lighting {
    /// Lit where the mesh has normals, else prelit.
    #[default]
    Auto,
    /// Always prelit (vertex colours); required in the field.
    Prelit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Multiplies positions (model units per mesh unit).
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub lighting: Lighting,
}

fn one() -> f32 {
    1.0
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { scale: 1.0, lighting: Lighting::Auto }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub sizes: Sizes,
    /// Primitive class → count, e.g. "3400".
    pub classes: BTreeMap<String, u32>,
    pub triangles: u32,
    pub quads: u32,
    /// Faces dropped (welded into a repeated corner; OBJ faces are always kept).
    pub dropped: u32,
    /// UVs that fell off the texture page and were clamped to its edge.
    pub clamped_uvs: u32,
    /// Coordinates outside ±4096 (the engine is fine to ±32767, but characters stay small).
    pub large: bool,
    pub vertices_before: u32,
}

fn q16(v: f32) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

/// One TMD object from `mesh`. `bind` maps the mesh's textured faces onto
/// the model's own texture; materials named `t###_c####` pick a page and
/// CLUT of their own (UVs over the whole page).
///
/// `reference` is the model being replaced: a gouraud face over the same
/// vertices whose vertex colours weren't repainted keeps its own corner
/// colours (OBJ holds one colour per vertex; retail faces often colour a
/// shared vertex differently).
pub fn encode(mesh: &InMesh, bind: Option<TexBind>, opt: &ImportOptions, reference: Option<&Object>) -> Result<(Object, ImportReport)> {
    encode_mapped(mesh, bind, opt, reference).map(|(o, r, _)| (o, r))
}

/// [`encode`], plus the vertex each mesh position became (for morph targets).
pub fn encode_mapped(mesh: &InMesh, bind: Option<TexBind>, opt: &ImportOptions, reference: Option<&Object>) -> Result<(Object, ImportReport, Vec<usize>)> {
    let reference = reference.filter(|_| mesh.indexed);
    let ref_decoded: Vec<Option<Decoded>> = reference.map(|r| r.prims.iter().map(decode).collect()).unwrap_or_default();
    let ref_vcol = reference.and_then(|r| vertex_colours(r, &ref_decoded));
    let mut ref_faces: HashMap<Vec<usize>, usize> = HashMap::new();
    for (i, d) in ref_decoded.iter().enumerate() {
        if let Some(d) = d.as_ref().filter(|d| d.grd) {
            let mut key = d.verts[..d.n].to_vec();
            key.sort_unstable();
            ref_faces.entry(key).or_insert(i);
        }
    }
    let mut o = Object::default();
    let mut report = ImportReport {
        sizes: Sizes { vertices: 0, normals: 0, primitives: 0, file: 0, built: 0 },
        classes: BTreeMap::new(),
        triangles: 0,
        quads: 0,
        dropped: 0,
        clamped_uvs: 0,
        large: false,
        vertices_before: 0,
    };
    // Positions in Y down; an OBJ keeps every vertex in order (pose morphs
    // index them), other meshes are welded by quantised position.
    let mut vmap: HashMap<[i16; 3], usize> = HashMap::new();
    let mut vid = vec![0usize; mesh.positions.len()];
    for (i, p) in mesh.positions.iter().enumerate() {
        let q = [q16(p[0] * opt.scale), q16(-p[1] * opt.scale), q16(p[2] * opt.scale)];
        if q.iter().any(|c| c.unsigned_abs() > 4096) {
            report.large = true;
        }
        vid[i] = if mesh.indexed {
            o.verts.push([q[0], q[1], q[2], 0]);
            o.verts.len() - 1
        } else {
            *vmap.entry(q).or_insert_with(|| {
                o.verts.push([q[0], q[1], q[2], 0]);
                o.verts.len() - 1
            })
        };
    }
    if o.verts.len() > 0xFFFF {
        return Err(bad("MODEL_TOO_BIG", format!("{} vertices; indices are 16-bit.", o.verts.len())));
    }
    let mut nmap: HashMap<[i16; 3], usize> = HashMap::new();
    let mut normal_index = |n: [f32; 3], o: &mut Object| -> usize {
        // Only longer-than-unit normals are shortened: some retail models
        // (toy/23) light dimmer with half-length normals on purpose.
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let l = if len > 1.002 { len } else { 1.0 };
        let q = [q16(n[0] / l * 4096.0), q16(-n[1] / l * 4096.0), q16(n[2] / l * 4096.0)];
        *nmap.entry(q).or_insert_with(|| {
            o.normals.push([q[0], q[1], q[2], 0]);
            o.normals.len() - 1
        })
    };
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    for (fi, f) in mesh.faces.iter().enumerate() {
        let n = f.corners.len();
        let vs: Vec<usize> = f.corners.iter().map(|c| vid[c.pos]).collect();
        // A face with a repeated corner draws nothing at rest, but an OBJ
        // keeps it: a pose can pull its corners apart (welding made it, so drop it).
        if !mesh.indexed && (0..n).any(|i| (i + 1..n).any(|j| vs[i] == vs[j])) {
            report.dropped += 1;
            continue;
        }
        let mat = f.material.and_then(|m| mesh.materials.get(m));
        let has_uv = f.corners.iter().all(|c| c.uv.is_some());
        let named = mat.and_then(|m| parse_binding(&m.name));
        let textured = has_uv && (named.is_some() || mat.is_some_and(|m| m.textured) || (mat.is_none() && bind.is_some())) && (named.is_some() || bind.is_some());
        let semi = mat.is_some_and(|m| m.semi);
        let has_n = f.corners.iter().all(|c| c.normal.is_some());
        let lit = has_n && opt.lighting == Lighting::Auto;
        let cols: Vec<[u8; 3]> = match mat.and_then(|m| m.color) {
            Some(k) => vec![k; n],
            None => f.corners.iter().map(|c| c.color.map(|c| [byte(c[0]), byte(c[1]), byte(c[2])]).unwrap_or([128, 128, 128])).collect(),
        };
        let mut cols = cols;
        if let (Some(rv), true) = (&ref_vcol, mat.is_none_or(|m| m.color.is_none())) {
            let mut key = vs.clone();
            key.sort_unstable();
            // The face at the same place first (two faces can share corners: front and back).
            let same = |d: &&Decoded| {
                let mut k = d.verts[..d.n].to_vec();
                k.sort_unstable();
                d.grd && k == key
            };
            let at = ref_decoded.get(fi).and_then(|d| d.as_ref()).filter(same);
            if let Some(d) = at.or_else(|| ref_faces.get(&key).and_then(|&i| ref_decoded[i].as_ref())) {
                for (c, col) in cols.iter_mut().enumerate() {
                    if rv.get(vs[c]) == Some(col) {
                        // Loop corner c is TMD corner 0-1-3-2 on a quad; else any corner on that vertex.
                        let z = if n == 4 { [0, 1, 3, 2][c] } else { c };
                        let k = if d.verts.get(z) == Some(&vs[c]) { Some(z) } else { (0..d.n).find(|&k| d.verts[k] == vs[c]) };
                        if let Some(k) = k {
                            *col = d.colours[k];
                        }
                    }
                }
            }
        }
        let grd = mat.is_some_and(|m| m.gouraud && m.color.is_none()) || cols.iter().any(|c| *c != cols[0]);
        let nidx: Vec<usize> = if lit { f.corners.iter().map(|c| normal_index(c.normal.expect("checked"), &mut o)).collect() } else { Vec::new() };
        let smooth = lit && (mat.is_some_and(|m| m.smooth) || nidx.iter().any(|&k| k != nidx[0]));
        let quad = n == 4;
        // Class: research 02 §1.2.
        let (mut mode, flag): (u8, u8) = match (textured, lit) {
            (true, true) => (if smooth { 0x34 } else { 0x24 }, 0x00),
            (true, false) => (if grd { 0x35 } else { 0x25 }, 0x01),
            (false, true) => (if smooth { 0x30 } else { 0x20 }, if grd { 0x04 } else { 0x00 }),
            (false, false) => (if grd { 0x31 } else { 0x21 }, 0x01),
        };
        if quad {
            mode |= 0x08;
        }
        if semi {
            mode |= 0x02;
        }
        // TMD corner order: loop 0-1-2-3 → Z order 0-1-3-2.
        let order: Vec<usize> = if quad { vec![0, 1, 3, 2] } else { vec![0, 1, 2] };
        let mut words: Vec<u32> = Vec::new();
        if textured {
            let (tsb, cba, b) = match named {
                Some((tsb, cba)) if bind.is_none_or(|b| (b.tsb, b.cba) != (tsb, cba)) => (tsb, cba, TexBind::page(tsb, cba)),
                _ => {
                    let b = bind.expect("textured needs a binding");
                    (b.tsb, b.cba, b)
                }
            };
            for (k, &c) in order.iter().enumerate() {
                let uv = f.corners[c].uv.expect("checked");
                let (u, v) = b.texel(uv).unwrap_or_else(|| {
                    report.clamped_uvs += 1;
                    let (u, v) = (b.u0 as f32 + uv[0] * b.w as f32, b.v0 as f32 + (1.0 - uv[1]) * b.h as f32);
                    (u.clamp(0.0, 255.0) as u8, v.clamp(0.0, 255.0) as u8)
                });
                let extra = match k {
                    0 => cba,
                    1 => tsb,
                    _ => 0,
                };
                words.push(u as u32 | (v as u32) << 8 | (extra as u32) << 16);
            }
        }
        let colour_words = match (lit, textured) {
            (true, true) => 0,
            (true, false) => {
                if grd {
                    order.len()
                } else {
                    1
                }
            }
            (false, _) => {
                if mode & 0x10 != 0 {
                    order.len()
                } else {
                    1
                }
            }
        };
        // An untextured class keeps its GPU code in the first colour word's top byte.
        for (k, &c) in order.iter().take(colour_words).enumerate() {
            let col = cols[c];
            let top = if k == 0 && !textured { mode as u32 } else { 0 };
            words.push(col[0] as u32 | (col[1] as u32) << 8 | (col[2] as u32) << 16 | top << 24);
        }
        let v = |k: usize| vs[order[k]] as u32;
        if lit {
            if smooth {
                for k in 0..order.len() {
                    words.push(nidx[order[k]] as u32 | v(k) << 16);
                }
            } else {
                words.push(nidx[0] as u32 | v(0) << 16);
                words.push(v(1) | v(2) << 16);
                if quad {
                    words.push(v(3));
                }
            }
        } else {
            words.push(v(0) | v(1) << 16);
            words.push(v(2) | if quad { v(3) << 16 } else { 0 });
        }
        let (ilen, olen) = class_words(mode, flag);
        debug_assert_eq!(ilen as usize, words.len(), "class {mode:02X}{flag:02X}");
        if ilen as usize != words.len() {
            return Err(bad("MODEL_ENCODE", format!("Class {mode:02X}{flag:02X} wrote {} words, expected {ilen}.", words.len())));
        }
        *report.classes.entry(format!("{:02X}{:02X}", mode, flag)).or_default() += 1;
        if quad {
            report.quads += 1;
        } else {
            report.triangles += 1;
        }
        o.prims.push(Prim { olen, flag, mode, words });
    }
    if o.prims.is_empty() {
        return Err(bad("MESH_EMPTY", "No usable faces."));
    }
    let tmd = Tmd::new(vec![o.clone()]);
    report.sizes = tmd.sizes();
    Ok((o, report, vid))
}

/// `name` = `t%03x_c%04x…` → (tsb, cba).
fn parse_binding(name: &str) -> Option<(u16, u16)> {
    let l = name.to_ascii_lowercase();
    let t = u16::from_str_radix(l.get(1..4)?, 16).ok()?;
    let c = u16::from_str_radix(l.get(6..10)?, 16).ok()?;
    (l.starts_with('t') && &l[4..6] == "_c").then_some((t, c))
}

/* ───────────────────────── Poses ───────────────────────── */

/// A pose block with `poses` poses that each move nothing: one record per
/// pose, a zero delta on vertex 0. The count can't be 0:
/// `ApplyWeightedVertexDeltas` is a do-while (`addi count,-1; bnez`), so an
/// empty record runs 2³² times.
pub fn neutral_poses(poses: u32) -> Vec<u8> {
    super::anim::write_poses(&vec![super::anim::Pose::neutral(); poses as usize])
}

/// The pose count of a pose block.
pub fn pose_count(b: &[u8]) -> u32 {
    if b.len() >= 4 {
        le32(b, 0)
    } else {
        0
    }
}

/* ───────────────────────── Containers ───────────────────────── */

pub use crate::mesh::Container;

/// The file and container behind a model id: `toy/22`, `enemy/17`,
/// `knight/0`, `knight/1` = KNIGHT02 = `enemy/0`, `pb/0`, `bf/3`,
/// `sys/6#knight`, `sys/6#baby` (the ids screen 06 lists).
pub fn source(id: &str) -> Option<(String, Container)> {
    if let Some(sub) = id.strip_prefix("sys/6#") {
        return match sub {
            "knight" => Some(("sys/6".into(), Container::Sys6Knight)),
            "baby" => Some(("sys/6".into(), Container::Sys6Baby)),
            _ => None,
        };
    }
    let (kind, n) = id.split_once('/')?;
    let n: u32 = n.parse().ok()?;
    Some(match kind {
        "toy" => (id.into(), Container::Toy),
        "enemy" if n < 2 => (id.into(), Container::Knight),
        "enemy" => (id.into(), Container::Enemy),
        "knight" if n == 0 => (id.into(), Container::Knight),
        "knight" if n <= 2 => (format!("enemy/{}", n - 1), Container::Knight),
        "pb" => (id.into(), Container::Pb),
        "bf" => (id.into(), Container::Bf),
        _ => return None,
    })
}

/// Section roles of the containers a model can be imported into:
/// (sections, poses, model, own TIM). Arenas (`bf/n`) have no section
/// table and no poses; they aren't characters.
fn layout(kind: Container) -> Option<(usize, usize, usize, Option<usize>)> {
    Some(match kind {
        Container::Toy => (8, 0, 7, Some(6)),
        Container::Enemy => (7, 0, 5, Some(3)),
        Container::Knight => (4, 0, 1, Some(2)),
        Container::Pb => (6, 0, 4, None),
        Container::Sys6Knight => (4, 2, 0, None),
        Container::Sys6Baby => (4, 3, 1, None),
        Container::Bf => return None,
    })
}

/// Drawn in the field (no light rig for models): imports are prelit.
pub fn is_field(kind: Container) -> bool {
    matches!(kind, Container::Sys6Knight | Container::Sys6Baby)
}

fn unsupported(kind: Container) -> EditError {
    bad("MODEL_TARGET", format!("{kind:?} files can't take an imported model."))
}

/// A container's sections as byte vectors (each runs to the next one in
/// file order, so padding stays with the section before it).
fn split(b: &[u8], count: usize) -> Result<Vec<Vec<u8>>> {
    let s = crate::container::sections(b, count).ok_or_else(|| bad("CONTAINER_BAD", "The file's section table doesn't parse."))?;
    Ok(s.iter().map(|x| b[x.offset as usize..(x.offset + x.len) as usize].to_vec()).collect())
}

/// Rebuilds a container from its sections in their original file order.
/// Each section keeps its original offset shifted by how much the sections
/// before it grew or shrank, with its original alignment (mod 4), so an
/// unchanged container rebuilds byte for byte. The offset table is
/// rewritten and the file padded to whole sectors.
fn join(original: &[u8], count: usize, parts: &[Vec<u8>]) -> Result<Vec<u8>> {
    let s = crate::container::sections(original, count).ok_or_else(|| bad("CONTAINER_BAD", "The file's section table doesn't parse."))?;
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&i| (s[i].offset, i));
    let header_end = s[order[0]].offset as usize;
    let mut out = original[..header_end].to_vec();
    let mut offsets = vec![0u32; count];
    let mut delta: i64 = 0;
    for &i in &order {
        let orig = s[i].offset as i64;
        let mut at = (orig + delta).max(out.len() as i64);
        while at.rem_euclid(4) != orig.rem_euclid(4) {
            at += 1;
        }
        out.resize(at as usize, 0);
        offsets[i] = at as u32;
        out.extend_from_slice(&parts[i]);
        delta = out.len() as i64 - (orig + s[i].len as i64);
    }
    for (i, o) in offsets.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&o.to_le_bytes());
    }
    out.resize(out.len().div_ceil(2048) * 2048, 0);
    Ok(out)
}

/// A limit the engine checks when it loads the file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    pub what: String,
    pub used: u32,
    pub cap: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Replaced {
    pub report: ImportReport,
    pub file_bytes: u32,
    pub retail_bytes: u32,
    /// The vertex count matched, so the poses were kept.
    pub poses_kept: bool,
    pub poses: u32,
    /// Poses written from the mesh's morph targets.
    pub morphs: Option<super::anim::FramesReport>,
    pub budgets: Vec<Budget>,
}

/// The main model of `file` and its own texture binding.
pub fn model_of(kind: Container, file: &[u8]) -> Result<(Tmd, Option<TexBind>)> {
    let parts = crate::mesh::model_parts(kind, file).ok_or_else(|| bad("CONTAINER_BAD", "The file's section table doesn't parse."))?;
    let tmd = Tmd::parse(parts.main)?;
    let bind = dominant_binding(&tmd).map(|(tsb, cba)| match parts.tim {
        Some(t) => TexBind::of_tim(tsb, cba, t),
        None => TexBind::page(tsb, cba),
    });
    Ok((tmd, bind))
}

/// The loader limits for `kind` with `file` (already rebuilt) and its
/// model's built size.
fn budgets(kind: Container, file: &[u8], built: u32, retail: usize) -> Vec<Budget> {
    let mut out = Vec::new();
    match kind {
        Container::Toy => {
            out.push(Budget { what: "Staging buffer (file size over)".into(), used: file.len() as u32, cap: 0x20000 });
        }
        // Baby forms load at 0x80101800, below OVR03 at 0x80139000.
        Container::Pb => {
            out.push(Budget { what: "Baby form area (below OVR03)".into(), used: file.len() as u32, cap: 0x37800 });
        }
        // Enemies and battle Knights load into the battle heap (ctx->f54),
        // SYS13 into the K&B heap beside other files; neither headroom is
        // measured (retail enemies run past 0x20000): stay inside the
        // retail sectors.
        Container::Enemy | Container::Knight | Container::Sys6Knight | Container::Sys6Baby => {
            out.push(Budget { what: "Retail file size (heap headroom unmeasured)".into(), used: file.len() as u32, cap: retail as u32 });
        }
        Container::Bf => {}
    }
    if kind == Container::Toy {
        // Sections 0–4 are copied into the toy's slot with the built model:
        // 0x800D7800 + slot × 0xE000 ("LT packet size over").
        if let Some(s) = crate::container::sections(file, 8) {
            out.push(Budget { what: "Toy slot (LT packet size over)".into(), used: s[5].offset - s[0].offset + built, cap: 0xE000 });
        }
    }
    out
}

/// `file` with its main model replaced by `mesh`. A mesh with morph targets
/// (glTF shape keys) also writes the poses: target i → pose i + 1, the rest
/// of the old pose count still. Otherwise the poses are kept when the
/// vertex count matches, else rewritten to move nothing.
pub fn replace(kind: Container, file: &[u8], mesh: &InMesh, opt: &ImportOptions) -> Result<(Vec<u8>, Replaced)> {
    let (count, poses_at, model_at, _) = layout(kind).ok_or_else(|| unsupported(kind))?;
    let (old, bind) = model_of(kind, file)?;
    let mut opt = *opt;
    if is_field(kind) {
        opt.lighting = Lighting::Prelit;
    }
    let (obj, mut report, vid) = encode_mapped(mesh, bind, &opt, old.objects.first())?;
    report.vertices_before = old.objects.iter().map(|o| o.verts.len() as u32).sum();
    let mut parts = split(file, count)?;
    let poses = pose_count(&parts[poses_at]);
    let poses_kept = mesh.morphs.is_empty() && old.objects.len() == 1 && old.objects[0].verts.len() == obj.verts.len();
    let mut morph_report = None;
    if !mesh.morphs.is_empty() {
        let (mut new, r) = super::anim::poses_from_morphs(mesh, &vid, obj.verts.len(), opt.scale);
        if new.len() > 255 {
            return Err(bad("POSE_COUNT", format!("{} shape keys; sequences address 255 poses at most.", new.len())));
        }
        while new.len() < poses as usize {
            new.push(super::anim::Pose::neutral());
        }
        parts[poses_at] = super::anim::write_poses(&new);
        morph_report = Some(r);
    } else if !poses_kept {
        parts[poses_at] = neutral_poses(poses);
    }
    let mut tmd = old;
    let scale = tmd.objects.first().map_or(0, |o| o.scale);
    tmd.objects = vec![Object { scale, ..obj }];
    parts[model_at] = tmd.write();
    let out = join(file, count, &parts)?;
    let budgets = budgets(kind, &out, tmd.sizes().built, file.len());
    if let Some(b) = budgets.iter().find(|b| b.used > b.cap) {
        return Err(bad("MODEL_BUDGET", format!("{}: {} B used, the game stops at {} B. Use fewer polygons or vertices.", b.what, b.used, b.cap)));
    }
    let poses_after = pose_count(&split(&out, count)?[poses_at]);
    let rep = Replaced { report, file_bytes: out.len() as u32, retail_bytes: file.len() as u32, poses_kept, poses: poses_after, morphs: morph_report, budgets };
    Ok((out, rep))
}

/// A container section by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Poses,
    Model,
}

/// `file` with section `index` replaced (the container rebuilt around it).
pub fn replace_section_index(kind: Container, file: &[u8], index: usize, bytes: Vec<u8>) -> Result<Vec<u8>> {
    let (count, ..) = layout(kind).ok_or_else(|| unsupported(kind))?;
    let mut parts = split(file, count)?;
    *parts.get_mut(index).ok_or_else(|| bad("CONTAINER_BAD", format!("No section {index}.")))? = bytes;
    join(file, count, &parts)
}

/// `file` with its pose block or main model section replaced.
pub fn replace_section(kind: Container, file: &[u8], role: Role, bytes: Vec<u8>) -> Result<Vec<u8>> {
    let (_, poses, model, _) = layout(kind).ok_or_else(|| unsupported(kind))?;
    replace_section_index(kind, file, if role == Role::Poses { poses } else { model }, bytes)
}

/// The loader limits for a rebuilt `file` (`retail` = the original's size).
pub fn budgets_of(kind: Container, file: &[u8], retail: usize) -> Result<Vec<Budget>> {
    let (tmd, _) = model_of(kind, file)?;
    Ok(budgets(kind, file, tmd.sizes().built, retail))
}

/// The OBJ + MTL of `file`'s main model, and the own-texture binding the
/// UVs are relative to.
pub fn export(kind: Container, file: &[u8], name: &str) -> Result<(String, String, Option<TexBind>)> {
    let (tmd, bind) = model_of(kind, file)?;
    let (obj, mtl) = export_obj(&tmd, bind, name);
    Ok((obj, mtl, bind))
}

/// The model's own TIM: its offset in `file` and the palette (row, col)
/// its binding draws with (the texture viewer's PNG coordinates).
pub fn own_tim(kind: Container, file: &[u8]) -> Option<(usize, usize, usize)> {
    let (count, _, _, tim) = layout(kind)?;
    let at = crate::container::sections(file, count)?.get(tim?)?.offset as usize;
    let t = crate::tim::Tim::parse(file.get(at..)?)?;
    let (_, bind) = model_of(kind, file).ok()?;
    let (row, col) = match (bind, t.info.clut) {
        (Some(b), Some(c)) => {
            let (x, y) = ((b.cba & 0x3F) * 16, b.cba >> 6);
            let entries = if t.info.bpp == 8 { 256 } else { 16 };
            if x >= c.x && y >= c.y && y < c.y + c.h && x < c.x + c.w {
                ((y - c.y) as usize, ((x - c.x) / entries) as usize)
            } else {
                (0, 0)
            }
        }
        _ => (0, 0),
    };
    Some((at, row, col))
}

/// The model's own TIM as the PNG its exported UVs map onto.
pub fn texture_png(kind: Container, file: &[u8]) -> Result<Option<Vec<u8>>> {
    match own_tim(kind, file) {
        Some((at, row, col)) => super::teximport::export_png(file, at, row, col).map(Some),
        None => Ok(None),
    }
}

/* ───────────────────────── Models without a TIM ───────────────────────── */

/// One texture binding of a model that ships no TIM: its whole 256×256
/// page through its CLUT, as the PNG the MTL maps onto its materials.
#[derive(Debug, Clone)]
pub struct PagePng {
    pub tsb: u16,
    pub cba: u16,
    /// `stem.png` for the binding most polygons use, `stem_tXXX_cYYYY.png`
    /// for each other one.
    pub file: String,
    pub png: Vec<u8>,
}

/// The (tsb, cba) bindings of `tmd`'s textured polygons: the dominant one
/// ([`dominant_binding`]) first, then the rest in order.
fn bindings(tmd: &Tmd) -> Vec<(u16, u16)> {
    let mut all: Vec<(u16, u16)> = tmd.objects.iter().flat_map(|o| o.prims.iter().filter_map(decode).filter(|d| d.uvs.is_some()).map(|d| (d.tsb, d.cba))).collect();
    all.sort_unstable();
    all.dedup();
    if let Some(first) = dominant_binding(tmd) {
        all.retain(|&b| b != first);
        all.insert(0, first);
    }
    all
}

/// Page `tsb` through CLUT `cba` as a 256×256 PNG. Index 0 (transparent
/// black) and texels no file in `vram` uploads are clear; the STP bit is
/// ignored (`_st` materials carry semi-transparency).
fn page_png(vram: &crate::mesh::OwnedVram, tsb: u16, cba: u16) -> Result<Vec<u8>> {
    let mut rgba = Vec::with_capacity(256 * 256 * 4);
    for v in 0..=255u8 {
        for u in 0..=255u8 {
            let c = vram.texel_rgba(tsb, cba, u, v);
            // texel_rgba: alpha 0 = index 0, 64 = unowned hatch, 128/255 = a colour.
            rgba.extend_from_slice(&if matches!(c[3], 0 | 64) { [0; 4] } else { [c[0], c[1], c[2], 255] });
        }
    }
    super::obj::encode_png(256, 256, &rgba).ok_or_else(|| bad("PNG_WRITE", "The page PNG could not be written."))
}

/// A model that ships no TIM, exported ([`export_paged`]).
#[derive(Debug, Clone)]
pub struct PagedExport {
    pub obj: String,
    pub mtl: String,
    /// One per texture binding; none for an untextured model.
    pub pages: Vec<PagePng>,
    /// Polygons left out of the OBJ (`# left out:` lines): they index past
    /// the vertex pool. The 3D view drops them too.
    pub skipped: u32,
    /// Lit polygons written without normals (`# no normals:` lines): their
    /// normals lie past the end of the TMD.
    pub no_normals: u32,
}

/// OBJ + MTL of a model that ships no TIM (NPC Group A models, `npc/n#i`),
/// with one PNG per texture binding its polygons use, decoded from `vram`
/// (the VRAM the game draws it with). Every UV is relative to its whole
/// page, so each PNG maps straight onto its materials. Vertices are written
/// in TMD order, so pose blocks still line up. An untextured model (prelit,
/// vertex colours only) gets no PNG.
pub fn export_paged(tmd: &Tmd, vram: &crate::mesh::OwnedVram, name: &str) -> Result<PagedExport> {
    let pages: Vec<PagePng> = bindings(tmd)
        .into_iter()
        .enumerate()
        .map(|(i, (tsb, cba))| {
            let file = if i == 0 { format!("{name}.png") } else { format!("{name}_t{tsb:03x}_c{cba:04x}.png") };
            Ok(PagePng { tsb, cba, file, png: page_png(vram, tsb, cba)? })
        })
        .collect::<Result<_>>()?;
    let maps: Vec<((u16, u16), String)> = pages.iter().map(|p| ((p.tsb, p.cba), p.file.clone())).collect();
    let (obj, mtl, omitted) = write_obj(tmd, None, &maps, name);
    Ok(PagedExport { obj, mtl, pages, skipped: omitted.polygons, no_normals: omitted.normals })
}

#[cfg(test)]
mod tests {
    use super::super::meshin::parse_obj;
    use super::*;

    #[test]
    fn an_obj_encodes_to_the_classes_it_asks_for() {
        let src = "v 0 0 0\nv 100 0 0\nv 100 100 0\nv 0 100 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 1\nusemtl tex\nf 1/1/1 2/2/1 3/3/1 4/4/1\nusemtl flat\nf 1 2 3\n";
        let mut m = parse_obj(src, None).unwrap();
        m.materials[0].textured = true;
        let bind = TexBind { tsb: 0x0A, cba: 0x7F80, u0: 64, v0: 0, w: 64, h: 128 };
        let (o, rep) = encode(&m, Some(bind), &ImportOptions::default(), None).unwrap();
        assert_eq!(o.verts.len(), 4);
        assert_eq!(rep.classes.get("2C00"), Some(&1), "{:?}", rep.classes); // textured lit flat quad
        assert_eq!(rep.classes.get("2101"), Some(&1), "{:?}", rep.classes); // untextured prelit flat tri
        // Y negated: (0,100,0) → y −100.
        assert!(o.verts.iter().any(|v| v[1] == -100));
        // The quad's UVs land inside the TIM rect, page/CLUT in words 1/0.
        let p = &o.prims[0];
        assert_eq!(p.words[0] >> 16, 0x7F80);
        assert_eq!(p.words[1] >> 16, 0x0A);
        for k in 0..4 {
            let (u, v) = (p.words[k] & 0xFF, (p.words[k] >> 8) & 0xFF);
            assert!((64..128).contains(&u) && v < 128, "{u},{v}");
        }
        // Export → import keeps geometry and classes.
        let tmd = Tmd::new(vec![o.clone()]);
        let (obj, _) = export_obj(&tmd, Some(bind), "m");
        let back = parse_obj(&obj, None).unwrap();
        let (o2, rep2) = encode(&back, Some(bind), &ImportOptions::default(), None).unwrap();
        assert_eq!(o2.verts, o.verts);
        assert_eq!(rep2.classes, rep.classes);
        assert_eq!(o2.prims, o.prims);
    }

    #[test]
    fn prelit_lighting_drops_normals() {
        let src = "v 0 0 0 1 0 0\nv 100 0 0 0 1 0\nv 0 100 0 0 0 1\nvn 0 0 1\nf 1//1 2//1 3//1\n";
        let m = parse_obj(src, None).unwrap();
        let (o, rep) = encode(&m, None, &ImportOptions { scale: 1.0, lighting: Lighting::Prelit }, None).unwrap();
        assert!(o.normals.is_empty());
        assert_eq!(rep.classes.get("3101"), Some(&1), "{:?}", rep.classes);
    }
}
