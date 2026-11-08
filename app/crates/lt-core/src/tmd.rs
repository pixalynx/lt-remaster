//! Sony TMD models (ID 0x41). The engine accepts 20 primitive classes (plus
//! their semi-transparent twins) and silently drops anything else.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::container::le32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmdObject {
    pub vertices: u32,
    pub normals: u32,
    pub primitives: u32,
    pub scale: i32,
    /// Primitive class (`mode << 8 | flag`, e.g. "3C00") → count.
    pub classes: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmdStats {
    pub objects: Vec<TmdObject>,
    pub vertices: u32,
    pub normals: u32,
    pub primitives: u32,
    pub classes: BTreeMap<String, u32>,
}

/// Parses object headers and counts primitives by class.
pub fn stats(b: &[u8]) -> Option<TmdStats> {
    if b.len() < 12 || le32(b, 0) != 0x41 {
        return None;
    }
    let flags = le32(b, 4);
    let nobj = le32(b, 8) as usize;
    if nobj > 256 {
        return None;
    }
    let table = 12usize;
    // FIXP clear: offsets are relative to the object table.
    let rel = |o: u32| if flags & 1 == 0 { table + o as usize } else { o as usize };
    let mut objects = Vec::with_capacity(nobj);
    let mut total = BTreeMap::new();
    for i in 0..nobj {
        let o = table + i * 28;
        let field = |k: usize| le32(b, o + k * 4);
        let (n_vert, n_norm, prim_top, n_prim) = (field(1), field(3), field(4), field(5));
        let scale = field(6) as i32;
        let mut classes = BTreeMap::new();
        let mut p = rel(prim_top);
        for _ in 0..n_prim {
            let hdr = b.get(p..p + 4)?;
            let (ilen, flag, mode) = (hdr[1] as usize, hdr[2], hdr[3]);
            let class = format!("{mode:02X}{flag:02X}");
            *classes.entry(class.clone()).or_insert(0) += 1;
            *total.entry(class).or_insert(0) += 1;
            p += 4 + ilen * 4;
        }
        objects.push(TmdObject { vertices: n_vert, normals: n_norm, primitives: n_prim, scale, classes });
    }
    Some(TmdStats {
        vertices: objects.iter().map(|o| o.vertices).sum(),
        normals: objects.iter().map(|o| o.normals).sum(),
        primitives: objects.iter().map(|o| o.primitives).sum(),
        objects,
        classes: total,
    })
}

/// Human name for a primitive class, as the model viewer labels them.
pub fn class_label(class: &str) -> &'static str {
    let mode = u8::from_str_radix(class.get(0..2).unwrap_or("00"), 16).unwrap_or(0);
    let flag = u8::from_str_radix(class.get(2..4).unwrap_or("00"), 16).unwrap_or(0);
    let quad = mode & 0x08 != 0;
    let textured = mode & 0x04 != 0;
    let gouraud = mode & 0x10 != 0;
    let unlit = flag & 0x01 != 0;
    match (quad, textured, gouraud, unlit) {
        (false, false, true, false) => "G3 lit, gouraud",
        (true, false, true, false) => "G4 lit, gouraud",
        (false, true, _, false) => "GT3 lit, textured",
        (true, true, _, false) => "GT4 lit, textured",
        (false, false, _, true) => "G3 unlit",
        (true, false, _, true) => "G4 unlit",
        (false, true, _, true) => "GT3 unlit, textured",
        (true, true, _, true) => "GT4 unlit, textured",
        _ => "flat",
    }
}
