//! Meshes from outside the game (screen 06 §5, research 06 phase 1): Wavefront
//! OBJ and glTF 2.0 read into one plain mesh the TMD encoder ([`super::model`])
//! turns into a model.
//!
//! Conventions are the exporter's ([`super::model::export_obj`]) and the
//! chunk OBJ's ([`super::obj`]): Y up (the game's Y negated), model units,
//! counter-clockwise front faces. OBJ faces may be quads (kept as quads);
//! glTF primitives are triangles. Vertex colours: OBJ `v x y z r g b`,
//! glTF `COLOR_0` (0–1, where 128/255 is 1.0 modulation in the game).

use std::collections::HashMap;
use std::path::Path;

use super::{EditError, Result};

#[derive(Debug, Clone, Default)]
pub struct Material {
    pub name: String,
    /// Has a base colour texture (glTF) or a `map_Kd` (OBJ) or a `t###_c####` name.
    pub textured: bool,
    /// Alpha blending asked for (`_st` suffix or glTF `alphaMode: BLEND`).
    pub semi: bool,
    /// Per-corner colours even when they match (`_g`: the GRD bit some
    /// retail models set on one-colour faces).
    pub gouraud: bool,
    /// Smooth (per-corner normals) even when they match (`_sm`).
    pub smooth: bool,
    /// One colour for the whole face (`_kRRGGBB`, raw bytes: 80 = 1.0 on a
    /// texture); it wins over vertex colours.
    pub color: Option<[u8; 3]>,
}

/// One corner of a face.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Corner {
    pub pos: usize,
    pub normal: Option<[f32; 3]>,
    pub uv: Option<[f32; 2]>,
    pub color: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Default)]
pub struct Face {
    /// 3 or 4 corners, counter-clockwise seen from the front.
    pub corners: Vec<Corner>,
    pub material: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct InMesh {
    /// Y up, model units.
    pub positions: Vec<[f32; 3]>,
    pub faces: Vec<Face>,
    pub materials: Vec<Material>,
    /// Positions are authored vertices (OBJ): the encoder keeps each one, in
    /// order, so pose morphs still line up. glTF splits vertices at seams,
    /// so its positions are welded instead.
    pub indexed: bool,
    /// Morph targets (glTF shape keys): per target, a delta per position
    /// (Y up, mesh units).
    pub morphs: Vec<Vec<[f32; 3]>>,
    pub morph_names: Vec<String>,
}

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

impl Material {
    /// The material a name describes: `t###_c####` (page word, CLUT id),
    /// then any of `_st`, `_g`, `_sm`, `_kRRGGBB`, `_lit`. Blender's `.001`
    /// duplicate suffix is ignored.
    pub fn named(name: &str) -> Self {
        material_of(name)
    }
}

fn material_of(name: &str) -> Material {
    let lower = name.to_ascii_lowercase();
    let base = lower.split('.').next().unwrap_or("");
    let tokens: Vec<&str> = base.split('_').collect();
    let hex = |t: &str, n: usize| t.len() == n && t.bytes().all(|b| b.is_ascii_hexdigit());
    let tex = tokens.len() >= 2 && tokens[0].starts_with('t') && hex(&tokens[0][1..], 3) && tokens[1].starts_with('c') && hex(&tokens[1][1..], 4);
    let color = tokens.iter().find(|t| t.starts_with('k') && hex(&t[1..], 6)).and_then(|t| u32::from_str_radix(&t[1..], 16).ok()).map(|c| [(c >> 16) as u8, (c >> 8) as u8, c as u8]);
    Material { name: name.to_string(), textured: tex, semi: tokens.contains(&"st"), gouraud: tokens.contains(&"g"), smooth: tokens.contains(&"sm"), color }
}

/* ───────────────────────── OBJ ───────────────────────── */

/// Parses an OBJ (and the MTL it names, from `dir`, when present).
pub fn parse_obj(src: &str, dir: Option<&Path>) -> Result<InMesh> {
    let mut m = InMesh { indexed: true, ..InMesh::default() };
    let mut colors: Vec<Option<[f32; 3]>> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut mats: HashMap<String, usize> = HashMap::new();
    let mut current: Option<usize> = None;
    let mut textured_mtl: HashMap<String, bool> = HashMap::new();
    let num = |s: &str, line: usize| s.parse::<f32>().map_err(|_| bad("OBJ_BAD", format!("Line {line}: “{s}” is not a number.")));
    for (ln, raw) in src.lines().enumerate() {
        let line = ln + 1;
        let l = raw.split('#').next().unwrap_or("").trim();
        let mut it = l.split_whitespace();
        match it.next() {
            Some("v") => {
                let p: Vec<f32> = it.map(|s| num(s, line)).collect::<Result<_>>()?;
                if p.len() < 3 {
                    return Err(bad("OBJ_BAD", format!("Line {line}: a vertex needs x y z.")));
                }
                m.positions.push([p[0], p[1], p[2]]);
                colors.push((p.len() >= 6).then(|| [p[3], p[4], p[5]]));
            }
            Some("vt") => {
                let p: Vec<f32> = it.map(|s| num(s, line)).collect::<Result<_>>()?;
                uvs.push([p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0)]);
            }
            Some("vn") => {
                let p: Vec<f32> = it.map(|s| num(s, line)).collect::<Result<_>>()?;
                if p.len() < 3 {
                    return Err(bad("OBJ_BAD", format!("Line {line}: a normal needs x y z.")));
                }
                normals.push([p[0], p[1], p[2]]);
            }
            Some("mtllib") => {
                if let (Some(dir), Some(name)) = (dir, it.next()) {
                    if let Ok(mtl) = std::fs::read_to_string(dir.join(name)) {
                        let mut cur = String::new();
                        for ml in mtl.lines() {
                            let mut w = ml.split_whitespace();
                            match w.next() {
                                Some("newmtl") => cur = w.next().unwrap_or("").to_string(),
                                Some("map_Kd") => {
                                    textured_mtl.insert(cur.clone(), true);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Some("usemtl") => {
                let name = it.next().unwrap_or("").to_string();
                let idx = *mats.entry(name.clone()).or_insert_with(|| {
                    let mut mat = material_of(&name);
                    mat.textured |= textured_mtl.get(&name).copied().unwrap_or(false);
                    m.materials.push(mat);
                    m.materials.len() - 1
                });
                current = Some(idx);
            }
            Some("f") => {
                let mut corners = Vec::new();
                for tok in it {
                    let mut parts = tok.split('/');
                    let resolve = |s: Option<&str>, len: usize| -> Result<Option<usize>> {
                        match s.filter(|s| !s.is_empty()) {
                            None => Ok(None),
                            Some(s) => {
                                let i: i64 = s.parse().map_err(|_| bad("OBJ_BAD", format!("Line {line}: bad index “{s}”.")))?;
                                let k = if i < 0 { len as i64 + i } else { i - 1 };
                                if k < 0 || k as usize >= len {
                                    return Err(bad("OBJ_BAD", format!("Line {line}: index {i} is out of range.")));
                                }
                                Ok(Some(k as usize))
                            }
                        }
                    };
                    let pos = resolve(parts.next(), m.positions.len())?.ok_or_else(|| bad("OBJ_BAD", format!("Line {line}: a face corner needs a vertex.")))?;
                    let uv = resolve(parts.next(), uvs.len())?.map(|i| uvs[i]);
                    let normal = resolve(parts.next(), normals.len())?.map(|i| normals[i]);
                    corners.push(Corner { pos, normal, uv, color: colors[pos] });
                }
                match corners.len() {
                    3 | 4 => m.faces.push(Face { corners, material: current }),
                    n if n > 4 => {
                        // Fan larger polygons into triangles.
                        for k in 1..n - 1 {
                            m.faces.push(Face { corners: vec![corners[0], corners[k], corners[k + 1]], material: current });
                        }
                    }
                    _ => return Err(bad("OBJ_BAD", format!("Line {line}: a face needs 3 or more corners."))),
                }
            }
            _ => {}
        }
    }
    if m.faces.is_empty() {
        return Err(bad("MESH_EMPTY", "The OBJ has no faces."));
    }
    Ok(m)
}

/* ───────────────────────── glTF ───────────────────────── */

fn b64(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return Err(bad("GLTF_BAD", "A data: URI is not base64.")),
        };
        buf = buf << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

type Mat4 = [f32; 16];
const IDENTITY: Mat4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut o = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

fn trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> Mat4 {
    let [x, y, z, w] = q;
    let r = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
    ];
    [r[0] * s[0], r[1] * s[0], r[2] * s[0], 0.0, r[3] * s[1], r[4] * s[1], r[5] * s[1], 0.0, r[6] * s[2], r[7] * s[2], r[8] * s[2], 0.0, t[0], t[1], t[2], 1.0]
}

fn apply(m: &Mat4, p: [f32; 3], w: f32) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r] * w)
}

/// Reads glTF (`.gltf` with data: URIs or `.bin` files beside it in `dir`) or GLB.
pub fn parse_gltf(bytes: &[u8], dir: Option<&Path>) -> Result<InMesh> {
    let (json, bin): (serde_json::Value, Option<Vec<u8>>) = if bytes.starts_with(b"glTF") {
        let rd = |o: usize| u32::from_le_bytes(bytes.get(o..o + 4).unwrap_or(&[0; 4]).try_into().expect("4 bytes")) as usize;
        let mut p = 12;
        let mut json = None;
        let mut bin = None;
        while p + 8 <= bytes.len() {
            let (len, kind) = (rd(p), rd(p + 4));
            let data = bytes.get(p + 8..p + 8 + len).ok_or_else(|| bad("GLTF_BAD", "A GLB chunk runs past the end."))?;
            match kind {
                0x4E4F_534A => json = Some(serde_json::from_slice(data).map_err(|e| bad("GLTF_BAD", format!("GLB JSON: {e}.")))?),
                0x004E_4942 => bin = Some(data.to_vec()),
                _ => {}
            }
            p += 8 + len;
        }
        (json.ok_or_else(|| bad("GLTF_BAD", "The GLB has no JSON chunk."))?, bin)
    } else {
        (serde_json::from_slice(bytes).map_err(|e| bad("GLTF_BAD", format!("glTF JSON: {e}.")))?, None)
    };
    let arr = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let buffers: Vec<Vec<u8>> = arr(&json, "buffers")
        .iter()
        .enumerate()
        .map(|(i, b)| match b.get("uri").and_then(|u| u.as_str()) {
            None if i == 0 => bin.clone().ok_or_else(|| bad("GLTF_BAD", "Buffer 0 has no data.")),
            None => Err(bad("GLTF_BAD", format!("Buffer {i} has no data."))),
            Some(u) if u.starts_with("data:") => b64(u.split_once(',').map(|x| x.1).unwrap_or("")),
            Some(u) => {
                let d = dir.ok_or_else(|| bad("GLTF_BAD", format!("Buffer {u} is a separate file; open the .gltf from disk.")))?;
                std::fs::read(d.join(u)).map_err(|e| bad("GLTF_BAD", format!("{u}: {e}.")))
            }
        })
        .collect::<Result<_>>()?;
    let views = arr(&json, "bufferViews");
    let accessors = arr(&json, "accessors");
    let u = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64()).map(|x| x as usize);
    // Reads accessor `i` as rows of `n` f32 (normalised integers scaled to 0–1).
    let read = |i: usize| -> Result<(usize, Vec<f32>)> {
        let a = accessors.get(i).ok_or_else(|| bad("GLTF_BAD", format!("No accessor {i}.")))?;
        let count = u(a, "count").unwrap_or(0);
        let comps = match a.get("type").and_then(|t| t.as_str()).unwrap_or("SCALAR") {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            t => return Err(bad("GLTF_BAD", format!("Accessor type {t} is not supported."))),
        };
        let ct = u(a, "componentType").unwrap_or(5126);
        let normalized = a.get("normalized").and_then(|x| x.as_bool()).unwrap_or(false);
        let size = match ct {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(bad("GLTF_BAD", format!("Component type {ct} is not supported."))),
        };
        let vi = u(a, "bufferView").ok_or_else(|| bad("GLTF_BAD", "Sparse or empty accessors are not supported."))?;
        let v = views.get(vi).ok_or_else(|| bad("GLTF_BAD", format!("No buffer view {vi}.")))?;
        let buf = buffers.get(u(v, "buffer").unwrap_or(0)).ok_or_else(|| bad("GLTF_BAD", "Missing buffer."))?;
        let base = u(v, "byteOffset").unwrap_or(0) + u(a, "byteOffset").unwrap_or(0);
        let stride = u(v, "byteStride").unwrap_or(comps * size);
        let mut out = Vec::with_capacity(count * comps);
        for r in 0..count {
            for c in 0..comps {
                let o = base + r * stride + c * size;
                let s = buf.get(o..o + size).ok_or_else(|| bad("GLTF_BAD", "An accessor runs past its buffer."))?;
                let val = match ct {
                    5126 => f32::from_le_bytes(s.try_into().expect("4")),
                    5125 => u32::from_le_bytes(s.try_into().expect("4")) as f32,
                    5123 => {
                        let x = u16::from_le_bytes(s.try_into().expect("2")) as f32;
                        if normalized {
                            x / 65535.0
                        } else {
                            x
                        }
                    }
                    5122 => {
                        let x = i16::from_le_bytes(s.try_into().expect("2")) as f32;
                        if normalized {
                            (x / 32767.0).max(-1.0)
                        } else {
                            x
                        }
                    }
                    5121 => {
                        let x = s[0] as f32;
                        if normalized {
                            x / 255.0
                        } else {
                            x
                        }
                    }
                    _ => {
                        let x = s[0] as i8 as f32;
                        if normalized {
                            (x / 127.0).max(-1.0)
                        } else {
                            x
                        }
                    }
                };
                out.push(val);
            }
        }
        Ok((comps, out))
    };
    let mut m = InMesh::default();
    for mat in arr(&json, "materials") {
        let name = mat.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let mut x = material_of(&name);
        x.textured |= mat.pointer("/pbrMetallicRoughness/baseColorTexture").is_some();
        x.semi |= mat.get("alphaMode").and_then(|a| a.as_str()) == Some("BLEND");
        m.materials.push(x);
    }
    let meshes = arr(&json, "meshes");
    let nodes = arr(&json, "nodes");
    // Walk the scene's node tree with world matrices.
    let scene = u(&json, "scene").unwrap_or(0);
    let roots: Vec<usize> = json.pointer(&format!("/scenes/{scene}/nodes")).and_then(|r| r.as_array()).map(|r| r.iter().filter_map(|x| x.as_u64().map(|x| x as usize)).collect()).unwrap_or_else(|| (0..nodes.len()).collect());
    let mut stack: Vec<(usize, Mat4)> = roots.into_iter().map(|r| (r, IDENTITY)).collect();
    let mut seen = 0;
    while let Some((ni, parent)) = stack.pop() {
        seen += 1;
        if seen > 10_000 {
            return Err(bad("GLTF_BAD", "The node tree loops."));
        }
        let n = nodes.get(ni).ok_or_else(|| bad("GLTF_BAD", format!("No node {ni}.")))?;
        let f = |k: &str, d: Vec<f32>| n.get(k).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|v| v.as_f64().map(|v| v as f32)).collect::<Vec<f32>>()).unwrap_or(d);
        let local: Mat4 = match n.get("matrix") {
            Some(_) => f("matrix", IDENTITY.to_vec()).try_into().unwrap_or(IDENTITY),
            None => {
                let t = f("translation", vec![0.0; 3]);
                let r = f("rotation", vec![0.0, 0.0, 0.0, 1.0]);
                let s = f("scale", vec![1.0; 3]);
                trs([t[0], t[1], t[2]], [r[0], r[1], r[2], r[3]], [s[0], s[1], s[2]])
            }
        };
        let world = mul(&parent, &local);
        for c in n.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
            if let Some(c) = c.as_u64() {
                stack.push((c as usize, world));
            }
        }
        let Some(mi) = u(n, "mesh") else { continue };
        let mesh = meshes.get(mi).ok_or_else(|| bad("GLTF_BAD", format!("No mesh {mi}.")))?;
        for prim in mesh.get("primitives").and_then(|p| p.as_array()).into_iter().flatten() {
            if u(prim, "mode").unwrap_or(4) != 4 {
                return Err(bad("GLTF_BAD", "Only triangle primitives can become a model (export triangles, not lines or strips)."));
            }
            let attr = |k: &str| prim.pointer(&format!("/attributes/{k}")).and_then(|x| x.as_u64()).map(|x| x as usize);
            let (_, pos) = read(attr("POSITION").ok_or_else(|| bad("GLTF_BAD", "A primitive has no POSITION."))?)?;
            let nrm = attr("NORMAL").map(read).transpose()?;
            let uv = attr("TEXCOORD_0").map(read).transpose()?;
            let col = attr("COLOR_0").map(read).transpose()?;
            let count = pos.len() / 3;
            let base = m.positions.len();
            for k in 0..count {
                m.positions.push(apply(&world, [pos[k * 3], pos[k * 3 + 1], pos[k * 3 + 2]], 1.0));
            }
            // Shape keys: POSITION deltas per target, in the node's frame.
            let targets = prim.get("targets").and_then(|t| t.as_array()).cloned().unwrap_or_default();
            for (t, target) in targets.iter().enumerate() {
                while m.morphs.len() <= t {
                    m.morphs.push(vec![[0.0; 3]; base]);
                }
                let d = match target.get("POSITION").and_then(|x| x.as_u64()) {
                    Some(a) => read(a as usize)?.1,
                    None => vec![0.0; count * 3],
                };
                let morph = &mut m.morphs[t];
                morph.resize(base, [0.0; 3]);
                for k in 0..count {
                    morph.push(d.get(k * 3..k * 3 + 3).map_or([0.0; 3], |v| apply(&world, [v[0], v[1], v[2]], 0.0)));
                }
            }
            if m.morph_names.is_empty() {
                if let Some(names) = mesh.pointer("/extras/targetNames").and_then(|n| n.as_array()) {
                    m.morph_names = names.iter().map(|n| n.as_str().unwrap_or("").to_string()).collect();
                }
            }
            let idx: Vec<usize> = match u(prim, "indices") {
                Some(i) => read(i)?.1.into_iter().map(|x| x as usize).collect(),
                None => (0..count).collect(),
            };
            let corner = |k: usize| Corner {
                pos: base + k,
                normal: nrm.as_ref().map(|(_, n)| {
                    let v = apply(&world, [n[k * 3], n[k * 3 + 1], n[k * 3 + 2]], 0.0);
                    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
                    [v[0] / l, v[1] / l, v[2] / l]
                }),
                // glTF V runs down the image; the importer wants OBJ-style V up.
                uv: uv.as_ref().map(|(_, t)| [t[k * 2], 1.0 - t[k * 2 + 1]]),
                color: col.as_ref().map(|(c, v)| [v[k * c], v[k * c + 1], v[k * c + 2]]),
            };
            for t in idx.chunks(3) {
                if t.len() == 3 && t.iter().all(|&i| i < count) {
                    m.faces.push(Face { corners: vec![corner(t[0]), corner(t[1]), corner(t[2])], material: u(prim, "material") });
                }
            }
        }
    }
    if m.faces.is_empty() {
        return Err(bad("MESH_EMPTY", "The glTF has no triangles."));
    }
    let n = m.positions.len();
    for morph in &mut m.morphs {
        morph.resize(n, [0.0; 3]);
    }
    Ok(m)
}

/// Reads an OBJ, glTF or GLB file by its extension.
pub fn read_file(path: &Path) -> Result<InMesh> {
    let bytes = std::fs::read(path).map_err(|e| bad("IO", format!("{}: {e}", path.display())))?;
    let dir = path.parent();
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("obj") => parse_obj(&String::from_utf8_lossy(&bytes), dir),
        Some("gltf") | Some("glb") => parse_gltf(&bytes, dir),
        _ => Err(bad("MESH_FORMAT", "Open an .obj, .gltf or .glb file.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_with_quads_colours_and_materials() {
        let src = "v 0 0 0 1 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 1\nusemtl t00a_c7f80_st\nf 1/1/1 2/2/1 3/3/1 4/4/1\nusemtl flat\nf 1 2 3\n";
        let m = parse_obj(src, None).unwrap();
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.faces.len(), 2);
        assert_eq!(m.faces[0].corners.len(), 4);
        assert_eq!(m.faces[0].corners[0].color, Some([1.0, 0.0, 0.0]));
        assert_eq!(m.faces[0].corners[2].uv, Some([1.0, 1.0]));
        assert!(m.materials[0].textured && m.materials[0].semi);
        assert!(!m.materials[1].textured);
    }

    #[test]
    fn a_minimal_gltf_with_a_data_uri() {
        // One triangle, positions only, as a data: URI.
        let pos: Vec<u8> = [0f32, 0., 0., 1., 0., 0., 0., 1., 0.].iter().flat_map(|f| f.to_le_bytes()).collect();
        let mut s = String::new();
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for c in pos.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                if k <= c.len() {
                    s.push(T[((n >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":36,"uri":"data:application/octet-stream;base64,{s}"}}],"bufferViews":[{{"buffer":0,"byteLength":36}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],"nodes":[{{"mesh":0,"translation":[10,0,0]}}],"scenes":[{{"nodes":[0]}}]}}"#
        );
        let m = parse_gltf(json.as_bytes(), None).unwrap();
        assert_eq!(m.faces.len(), 1);
        assert_eq!(m.positions[1], [11.0, 0.0, 0.0]);
    }

    fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in bytes.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                s.push(if k <= c.len() { T[((n >> (18 - 6 * k)) & 63) as usize] as char } else { '=' });
            }
        }
        s
    }

    #[test]
    fn gltf_shape_keys_become_morphs() {
        // A triangle and one shape key that lifts vertex 2 by 5 (node scaled ×2).
        let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let mut buf = f(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
        buf.extend(f(&[0., 0., 0., 0., 0., 0., 0., 5., 0.]));
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":72,"uri":"data:application/octet-stream;base64,{}"}}],"bufferViews":[{{"buffer":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":36}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}},{{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"targets":[{{"POSITION":1}}]}}],"extras":{{"targetNames":["lift"]}}}}],"nodes":[{{"mesh":0,"scale":[2,2,2]}}],"scenes":[{{"nodes":[0]}}]}}"#,
            b64(&buf)
        );
        let m = parse_gltf(json.as_bytes(), None).unwrap();
        assert_eq!(m.morphs.len(), 1);
        assert_eq!(m.morph_names, vec!["lift".to_string()]);
        assert_eq!(m.morphs[0][2], [0.0, 10.0, 0.0]);
        assert_eq!(m.morphs[0][0], [0.0; 3]);
        // As a pose: Y up 10 → TMD Y −10 on welded vertex 2.
        let (o, _, vid) = super::super::model::encode_mapped(&m, None, &Default::default(), None).unwrap();
        let (poses, rep) = super::super::anim::poses_from_morphs(&m, &vid, o.verts.len(), 1.0);
        assert_eq!(poses[0].dense(3)[vid[2]], [0, -10, 0]);
        assert_eq!(rep.max_delta, 10);
    }
}
