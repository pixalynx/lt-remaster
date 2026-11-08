//! TMD models as the game holds them in RAM, back in file form.
//!
//! A TMD's object table has three pointers per object (vertices, normals, primitives). In a file
//! they're offsets from the table (flags bit 0 clear); `Model_BuildPrimPackets` and its variants
//! relocate them to RAM addresses in place and set the bit. The field Knight's model is drawn
//! from a copy and is never relocated. The primitives (UVs, colours, texture pages, indices) are
//! never written, so a model's [`signature`] is the same in RAM as on the disc.

/// The TMD id word.
pub const TMD_ID: u32 = 0x41;
/// Bytes of an object table entry: vertices, count, normals, count, primitives, count, scale.
pub const OBJECT_BYTES: usize = 28;
/// More objects than any game model has (`tmd_mesh` stops at 256).
const MAX_OBJECTS: usize = 64;

pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// FNV-1a, 64-bit.
pub fn fnv(bytes: &[u8]) -> u64 {
    fnv_on(0xCBF2_9CE4_8422_2325, bytes)
}

pub(crate) fn fnv_on(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01B3);
    }
    h
}

/// One object of a file-form TMD: where its data is (offsets from the start of the file) and how
/// many of each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Object {
    pub verts: usize,
    pub n_verts: u32,
    pub normals: usize,
    pub n_normals: u32,
    pub prims: usize,
    pub n_prims: u32,
    /// Bytes of its primitives.
    pub prim_bytes: usize,
}

/// Bytes of `n` primitives starting at `at` (each is a 4-byte header, then `ilen` words).
fn prim_len(b: &[u8], at: usize, n: u32) -> Option<usize> {
    let mut p = at;
    for _ in 0..n {
        let ilen = *b.get(p + 1)? as usize;
        p += 4 + ilen * 4;
    }
    (p <= b.len()).then_some(p - at)
}

/// The objects of a file-form TMD (pointers relative to the table, flags bit 0 clear).
pub fn objects(b: &[u8]) -> Option<Vec<Object>> {
    if b.len() < 12 || le32(b, 0) != TMD_ID || le32(b, 4) & 1 != 0 {
        return None;
    }
    let n = le32(b, 8) as usize;
    if n == 0 || n > MAX_OBJECTS || 12 + n * OBJECT_BYTES > b.len() {
        return None;
    }
    (0..n)
        .map(|o| {
            let w = |k: usize| le32(b, 12 + o * OBJECT_BYTES + k * 4);
            let prims = 12 + w(4) as usize;
            let n_prims = w(5);
            Some(Object {
                verts: 12 + w(0) as usize,
                n_verts: w(1),
                normals: 12 + w(2) as usize,
                n_normals: w(3),
                prims,
                n_prims,
                prim_bytes: prim_len(b, prims, n_prims)?,
            })
        })
        .collect()
}

/// What identifies a model: per object its vertex, normal and primitive counts and its
/// primitives' bytes. Vertex positions are left out (a pose may move them in place).
pub fn signature(b: &[u8]) -> Option<u64> {
    let objs = objects(b)?;
    let mut h = fnv(b"tmd");
    for o in &objs {
        for v in [o.n_verts, o.n_normals, o.n_prims] {
            h = fnv_on(h, &v.to_le_bytes());
        }
        h = fnv_on(h, b.get(o.prims..o.prims + o.prim_bytes)?);
    }
    Some(h)
}

/// Reads guest memory: main RAM (any segment) or the scratchpad.
pub trait Ram {
    fn read(&self, addr: u32, len: usize) -> Vec<u8>;
}

impl<F: Fn(u32, usize) -> Vec<u8>> Ram for F {
    fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        self(addr, len)
    }
}

/// Bytes of `n` primitives at `at` in RAM, read in growing blocks.
fn ram_prims(ram: &dyn Ram, at: u32, n: u32) -> Option<Vec<u8>> {
    let mut len = 0x1000usize;
    loop {
        let b = ram.read(at, len);
        let mut p = 0usize;
        let mut whole = true;
        for _ in 0..n {
            match b.get(p + 1) {
                Some(&ilen) => p += 4 + ilen as usize * 4,
                None => {
                    whole = false;
                    break;
                }
            }
        }
        if whole && p <= b.len() {
            return Some(b[..p].to_vec());
        }
        if len >= 0x4_0000 {
            return None;
        }
        len *= 4;
    }
}

/// The TMD at `at` in RAM in file form: header, object table with offsets, then each object's
/// vertices, normals and primitives. `None` if it isn't a TMD.
pub fn from_ram(ram: &dyn Ram, at: u32) -> Option<Vec<u8>> {
    let hdr = ram.read(at, 12);
    if hdr.len() < 12 || le32(&hdr, 0) != TMD_ID {
        return None;
    }
    let relocated = le32(&hdr, 4) & 1 != 0;
    let n = le32(&hdr, 8) as usize;
    if n == 0 || n > MAX_OBJECTS {
        return None;
    }
    let table = ram.read(at + 12, n * OBJECT_BYTES);
    let addr = |p: u32| if relocated { p } else { at + 12 + p };
    let mut out = Vec::with_capacity(0x2000);
    out.extend_from_slice(&TMD_ID.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.resize(12 + n * OBJECT_BYTES, 0);
    for o in 0..n {
        let w = |k: usize| le32(&table, o * OBJECT_BYTES + k * 4);
        let (verts, n_verts, normals, n_normals, prims, n_prims, scale) =
            (w(0), w(1), w(2), w(3), w(4), w(5), w(6));
        if n_verts > 0x1000 || n_normals > 0x1000 || n_prims > 0x1000 {
            return None;
        }
        let mut entry = [0u32; 7];
        entry[0] = (out.len() - 12) as u32;
        out.extend(ram.read(addr(verts), n_verts as usize * 8));
        entry[1] = n_verts;
        entry[2] = (out.len() - 12) as u32;
        out.extend(ram.read(addr(normals), n_normals as usize * 8));
        entry[3] = n_normals;
        entry[4] = (out.len() - 12) as u32;
        out.extend(ram_prims(ram, addr(prims), n_prims)?);
        entry[5] = n_prims;
        entry[6] = scale;
        for (k, v) in entry.iter().enumerate() {
            let at = 12 + o * OBJECT_BYTES + k * 4;
            out[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-object TMD: 3 vertices, 1 normal, one flat triangle (mode 0x20, ilen 3).
    fn tmd() -> Vec<u8> {
        let mut b = Vec::new();
        for w in [TMD_ID, 0, 1] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        // verts at table+28, 3; normals at +52, 1; prims at +60, 1.
        for w in [28u32, 3, 52, 1, 60, 1, 0] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        for v in [
            [0i16, 0, 0, 0],
            [100, 0, 0, 0],
            [0, 100, 0, 0],
            [0, 0, 4096, 0],
        ] {
            for c in v {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        b.extend_from_slice(&[3, 3, 0, 0x20]);
        for w in [0x0080_8080u32, 0x0001_0000, 0x0002_0000] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b
    }

    #[test]
    fn a_relocated_model_reads_back_as_its_file() {
        let file = tmd();
        let at = 0x8011_0000u32;
        // In RAM: relocated, so pointers are addresses and flags bit 0 is set.
        let mut ram = file.clone();
        ram[4] = 1;
        for k in [0usize, 2, 4] {
            let o = 12 + k * 4;
            let v = le32(&ram, o) + at + 12;
            ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        let read = |a: u32, len: usize| -> Vec<u8> {
            let s = (a - at) as usize;
            ram.get(s..(s + len).min(ram.len()))
                .map(<[u8]>::to_vec)
                .unwrap_or_default()
        };
        let back = from_ram(&read, at).expect("a TMD");
        assert_eq!(back, file);
        assert_eq!(signature(&back), signature(&file));
        assert_eq!(objects(&file).unwrap()[0].prim_bytes, 16);
        // Unrelocated (the field Knight): read as is.
        let read = |a: u32, len: usize| -> Vec<u8> {
            let s = (a - at) as usize;
            file.get(s..(s + len).min(file.len()))
                .map(<[u8]>::to_vec)
                .unwrap_or_default()
        };
        assert_eq!(from_ram(&read, at).unwrap(), file);
    }

    #[test]
    fn moving_a_vertex_keeps_the_signature() {
        let a = tmd();
        let mut b = a.clone();
        b[12 + 28 + 8] = 7;
        assert_eq!(signature(&a), signature(&b));
        let mut c = a.clone();
        let last = c.len() - 1;
        c[last] ^= 1;
        assert_ne!(signature(&a), signature(&c));
    }
}
