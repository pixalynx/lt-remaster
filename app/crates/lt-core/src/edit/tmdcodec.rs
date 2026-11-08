//! TMD models as editable data (research 02 §1, screen 06 §5): every object's
//! vertices, normals and primitive packets, parsed so that writing them back
//! reproduces the file byte for byte, and so that an importer can build a new
//! model from scratch ([`super::model`]).
//!
//! ```text
//! +0  u32 0x41, u32 flags (0 on disc), u32 nobj
//! +12 nobj × {u32 vert_top, n_vert, normal_top, n_normal, prim_top, n_prim; s32 scale}
//!     offsets relative to +12; retail order: primitives of every object,
//!     then each object's vertices, then its normals
//! ```
//!
//! A primitive is a header word `{u8 olen, u8 ilen, u8 flag, u8 mode}` and
//! `ilen` data words; the engine builds only the 20 classes of
//! [`crate::mesh::TMD_CLASSES`] (plus their semi-transparent twins).

use serde::Serialize;

use crate::container::{le32, le_i16};

use super::{EditError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prim {
    pub olen: u8,
    pub flag: u8,
    pub mode: u8,
    pub words: Vec<u32>,
}

impl Prim {
    /// `mode << 8 | flag`, ABE masked off (the engine's dispatch key).
    pub fn key(&self) -> u16 {
        ((self.mode as u16) << 8 | self.flag as u16) & !0x200
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Object {
    pub verts: Vec<[i16; 4]>,
    pub normals: Vec<[i16; 4]>,
    pub prims: Vec<Prim>,
    pub scale: i32,
}

/// Where each block sits, so a parsed file writes back as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Layout {
    /// Per object: (vert_top, normal_top, prim_top) as stored.
    tops: Vec<(u32, u32, u32)>,
    /// Bytes after the last block (sector padding, trailing data).
    tail: Vec<u8>,
    /// Declared normal counts that run past the file (toy/34 Minicar).
    short_normals: Vec<u32>,
    /// Per object: (vertices, normals, primitive bytes) when parsed.
    sizes: Vec<(usize, usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tmd {
    pub flags: u32,
    pub objects: Vec<Object>,
    layout: Option<Layout>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sizes {
    pub vertices: u32,
    pub normals: u32,
    pub primitives: u32,
    /// Bytes on disc.
    pub file: u32,
    /// Bytes the engine builds (packets, vertex copies, part records), research 02 §1.2.
    pub built: u32,
}

fn bad(m: impl Into<String>) -> EditError {
    EditError::new("TMD_BAD", m)
}

/// Built stride per class slot (R-record + 2 GPU primitives), research 02 §1.2.
pub const STRIDES: [u32; 20] = [80, 100, 104, 136, 76, 96, 92, 120, 60, 72, 84, 108, 84, 108, 92, 120, 52, 64, 68, 88];

/// Data words a class carries (`ilen`), and the GPU packet length (`olen`).
pub fn class_words(mode: u8, flag: u8) -> (u8, u8) {
    let n = if mode & 0x08 != 0 { 4u8 } else { 3 };
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
    let uvs = if textured { n } else { 0 };
    let index_words = if lit {
        if iip {
            n
        } else {
            // one normal + n vertex halves, packed in halfwords
            (1 + n).div_ceil(2)
        }
    } else {
        n.div_ceil(2)
    };
    let ilen = uvs + colours + index_words;
    // GPU packet words: tag + colour, n × xy, n × uv if textured, n − 1 more colours if
    // gouraud (smooth, or a flat lit polygon with per-vertex colours: GRD).
    let gouraud = iip || (lit && !textured && grd);
    let olen = 1 + n + if textured { n } else { 0 } + if gouraud { n - 1 } else { 0 };
    (ilen, olen)
}

impl Tmd {
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 12 || le32(b, 0) != 0x41 {
            return Err(bad("Not a TMD (ID 0x41)."));
        }
        let flags = le32(b, 4);
        let nobj = le32(b, 8) as usize;
        if nobj == 0 || nobj > 256 || 12 + nobj * 28 > b.len() {
            return Err(bad(format!("{nobj} objects don't fit.")));
        }
        let fixp = flags & 1 != 0;
        let rel = |o: u32| if fixp { o as usize } else { 12 + o as usize };
        let mut objects = Vec::with_capacity(nobj);
        let mut tops = Vec::with_capacity(nobj);
        let mut short_normals = Vec::new();
        let mut end = 12 + nobj * 28;
        for i in 0..nobj {
            let o = 12 + i * 28;
            let f = |k: usize| le32(b, o + k * 4);
            let (vt, nv, nt, nn, pt, np, scale) = (f(0), f(1) as usize, f(2), f(3) as usize, f(4), f(5) as usize, f(6) as i32);
            if nv > 0x10000 || nn > 0x10000 || np > 0x10000 {
                return Err(bad("Object counts out of range."));
            }
            let svec = |at: usize| [le_i16(b, at), le_i16(b, at + 2), le_i16(b, at + 4), le_i16(b, at + 6)];
            if rel(vt) + nv * 8 > b.len() {
                return Err(bad("Vertices past the end."));
            }
            let verts: Vec<[i16; 4]> = (0..nv).map(|k| svec(rel(vt) + k * 8)).collect();
            let fit = (b.len().saturating_sub(rel(nt))) / 8;
            short_normals.push(if nn > fit { nn as u32 } else { 0 });
            let normals: Vec<[i16; 4]> = (0..nn.min(fit)).map(|k| svec(rel(nt) + k * 8)).collect();
            let mut prims = Vec::with_capacity(np);
            let mut p = rel(pt);
            for _ in 0..np {
                let hdr = b.get(p..p + 4).ok_or_else(|| bad("Primitive past the end."))?;
                let (olen, ilen, flag, mode) = (hdr[0], hdr[1] as usize, hdr[2], hdr[3]);
                let words = (0..ilen).map(|k| b.get(p + 4 + k * 4..p + 8 + k * 4).map(|s| le32(s, 0))).collect::<Option<Vec<u32>>>().ok_or_else(|| bad("Primitive data past the end."))?;
                prims.push(Prim { olen, flag, mode, words });
                p += 4 + ilen * 4;
            }
            end = end.max(p).max(rel(vt) + nv * 8).max((rel(nt) + nn * 8).min(b.len()));
            tops.push((vt, nt, pt));
            objects.push(Object { verts, normals, prims, scale });
        }
        let tail = b[end.min(b.len())..].to_vec();
        let sizes = objects.iter().map(|o| (o.verts.len(), o.normals.len(), o.prims.iter().map(|p| 4 + p.words.len() * 4).sum())).collect();
        Ok(Self { flags, objects, layout: Some(Layout { tops, tail, short_normals, sizes }) })
    }

    /// A new TMD (canonical layout) from objects.
    pub fn new(objects: Vec<Object>) -> Self {
        Self { flags: 0, objects, layout: None }
    }

    /// Drops the stored layout, so [`Self::write`] lays the blocks out afresh.
    pub fn relayout(&mut self) {
        self.layout = None;
    }

    pub fn write(&self) -> Vec<u8> {
        if let Some(l) = &self.layout {
            if l.tops.len() == self.objects.len() && self.layout_still_fits(l) {
                return self.write_layout(l);
            }
        }
        self.write_canonical()
    }

    /// [`Self::write`] without the bytes after the last block (retail
    /// sections carry padding up to the next section).
    pub fn write_trimmed(&self) -> Vec<u8> {
        let mut out = self.write();
        if let Some(l) = &self.layout {
            if l.tops.len() == self.objects.len() && self.layout_still_fits(l) {
                out.truncate(out.len() - l.tail.len());
            }
        }
        out
    }

    /// The stored tops are reusable while every block keeps its size (objects
    /// may share a vertex or normal block, which writes the same bytes twice).
    fn layout_still_fits(&self, l: &Layout) -> bool {
        l.sizes.len() == self.objects.len()
            && self.objects.iter().zip(&l.sizes).all(|(o, &(nv, nn, pb))| {
                o.verts.len() == nv && o.normals.len() == nn && o.prims.iter().map(|p| 4 + p.words.len() * 4).sum::<usize>() == pb
            })
    }

    fn write_layout(&self, l: &Layout) -> Vec<u8> {
        let nobj = self.objects.len();
        let mut size = 12 + nobj * 28;
        // Empty blocks don't count: prelit effect objects declare normal blocks past the section's end.
        let end = |top: u32, len: usize| if len == 0 { 0 } else { 12 + top as usize + len };
        for (o, &(vt, nt, pt)) in self.objects.iter().zip(&l.tops) {
            size = size.max(end(vt, o.verts.len() * 8));
            size = size.max(end(nt, o.normals.len() * 8));
            size = size.max(end(pt, o.prims.iter().map(|p| 4 + p.words.len() * 4).sum::<usize>()));
        }
        let mut out = vec![0u8; size];
        out[0..4].copy_from_slice(&0x41u32.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..12].copy_from_slice(&(nobj as u32).to_le_bytes());
        for (i, (o, &(vt, nt, pt))) in self.objects.iter().zip(&l.tops).enumerate() {
            let short = l.short_normals.get(i).copied().filter(|&n| n as usize > o.normals.len());
            let head = [vt, o.verts.len() as u32, nt, short.unwrap_or(o.normals.len() as u32), pt, o.prims.len() as u32, o.scale as u32];
            for (k, v) in head.iter().enumerate() {
                out[12 + i * 28 + k * 4..12 + i * 28 + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            put_svecs(&mut out, 12 + vt as usize, &o.verts);
            put_svecs(&mut out, 12 + nt as usize, &o.normals);
            put_prims(&mut out, 12 + pt as usize, &o.prims);
        }
        out.extend_from_slice(&l.tail);
        out
    }

    fn write_canonical(&self) -> Vec<u8> {
        let nobj = self.objects.len();
        let table = 12 + nobj * 28;
        let prim_bytes: Vec<usize> = self.objects.iter().map(|o| o.prims.iter().map(|p| 4 + p.words.len() * 4).sum()).collect();
        let mut at = table;
        let mut tops = Vec::with_capacity(nobj);
        let mut prim_at = Vec::with_capacity(nobj);
        for pb in &prim_bytes {
            prim_at.push(at);
            at += pb;
        }
        for o in &self.objects {
            let vt = at;
            at += o.verts.len() * 8;
            let nt = at;
            at += o.normals.len() * 8;
            tops.push((vt, nt));
        }
        let mut out = vec![0u8; at];
        out[0..4].copy_from_slice(&0x41u32.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..12].copy_from_slice(&(nobj as u32).to_le_bytes());
        for (i, o) in self.objects.iter().enumerate() {
            let (vt, nt) = tops[i];
            let head = [(vt - 12) as u32, o.verts.len() as u32, (nt - 12) as u32, o.normals.len() as u32, (prim_at[i] - 12) as u32, o.prims.len() as u32, o.scale as u32];
            for (k, v) in head.iter().enumerate() {
                out[12 + i * 28 + k * 4..12 + i * 28 + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            put_svecs(&mut out, vt, &o.verts);
            put_svecs(&mut out, nt, &o.normals);
            put_prims(&mut out, prim_at[i], &o.prims);
        }
        out
    }

    pub fn sizes(&self) -> Sizes {
        let mut s = Sizes { vertices: 0, normals: 0, primitives: 0, file: self.write().len() as u32, built: 0 };
        for o in &self.objects {
            s.vertices += o.verts.len() as u32;
            s.normals += o.normals.len() as u32;
            s.primitives += o.prims.len() as u32;
            // Part record, two vertex copies (live + pristine), normals.
            s.built += 0x3C + 16 * o.verts.len() as u32 + 8 * o.normals.len() as u32;
            for p in &o.prims {
                if let Some(slot) = crate::mesh::TMD_CLASSES.iter().position(|&k| k == p.key()) {
                    s.built += STRIDES[slot];
                }
            }
        }
        s
    }
}

fn put_svecs(out: &mut [u8], at: usize, v: &[[i16; 4]]) {
    for (k, s) in v.iter().enumerate() {
        for c in 0..4 {
            out[at + k * 8 + c * 2..at + k * 8 + c * 2 + 2].copy_from_slice(&s[c].to_le_bytes());
        }
    }
}

fn put_prims(out: &mut [u8], at: usize, prims: &[Prim]) {
    let mut p = at;
    for pr in prims {
        out[p] = pr.olen;
        out[p + 1] = pr.words.len() as u8;
        out[p + 2] = pr.flag;
        out[p + 3] = pr.mode;
        for (k, w) in pr.words.iter().enumerate() {
            out[p + 4 + k * 4..p + 8 + k * 4].copy_from_slice(&w.to_le_bytes());
        }
        p += 4 + pr.words.len() * 4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canonical_tmd_parses_back() {
        let o = Object {
            verts: vec![[0, 0, 0, 0], [100, 0, 0, 0], [0, -100, 0, 0]],
            normals: vec![[0, 0, -4096, 0]],
            prims: vec![Prim { olen: 6, flag: 0x01, mode: 0x31, words: vec![0x0080_8080, 0x3080_8080, 0x0080_8080, 0x0001_0000, 0x0000_0002] }],
            scale: 0,
        };
        let t = Tmd::new(vec![o.clone()]);
        let b = t.write();
        let back = Tmd::parse(&b).unwrap();
        assert_eq!(back.objects, vec![o]);
        assert_eq!(back.write(), b);
    }
}
