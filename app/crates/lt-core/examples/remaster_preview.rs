//! Research 16 (a native remaster): retail field chunks as one binary glTF 2.0 file that a modern
//! renderer can light. The geometry and textures are the ones the editor's 3D view draws
//! (`mesh::chunk_mesh`, `mesh::field_vram`, `mesh::build_atlas`). What this adds is what the
//! disc doesn't hold: chunks carry baked vertex colours and no normals, so normals are computed
//! here (face normals, smoothed across corners within 50°).
//!
//! ```text
//! cargo run --release -p lt-core --example remaster_preview -- <disc.bin> <out.glb> <chunk> [radius]
//! ```
//!
//! `radius` takes neighbours on the overworld grid (0 = the chunk alone, the default; interiors
//! have no grid, so they always come alone). Units: 256 game units = 1 glTF unit, Y up.
//!
//! Primitives per chunk: textured and untextured, each opaque or semi-transparent. Textured
//! colours are the vertex colours over 128 (128 = the texture as is, up to 2× brighter); raw
//! texture polygons get white; untextured colours are over 255. They are written linear (^2.2),
//! as glTF wants, so a renderer that outputs sRGB multiplies them the way the PS1 did. Texel
//! 0x0000 is cut out.
use std::collections::HashMap;

use lt_core::worldmap::WorldDisc;
use lt_core::{mesh, overworld, world, DiscImage, FileTable, PsxExe};
use serde_json::{json, Value};

const SCALE: f32 = 1.0 / 256.0;
const SMOOTH_COS: f32 = 0.642_787_6; // cos 50°

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: remaster_preview <disc.bin> <out.glb> <chunk> [radius]");
        std::process::exit(2);
    }
    let image = DiscImage::open(&args[1]).expect("disc");
    let exe = PsxExe::from_disc(&image).expect("exe");
    let table = FileTable::parse(&exe, image.sectors()).expect("file table");
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let centre: u32 = args[3].parse().expect("chunk id");
    let radius: i32 = args.get(4).map(|r| r.parse().expect("radius")).unwrap_or(0);

    let ids = world::texset_ids(&exe).unwrap_or_default();
    let sys_atlas = wd.file("sys/7").ok();

    let mut chunks = vec![centre];
    if let (Some((gx, gy)), true) = (world::grid_cell(centre), radius > 0) {
        chunks.clear();
        for dy in -radius..=radius {
            for dx in -2 * radius..=2 * radius {
                let (x, y) = (gx as i32 + dx, gy as i32 + dy);
                if x < 0 || y < 0 {
                    continue;
                }
                if let Some(id) = world::chunk_at(x as u32, y as u32) {
                    chunks.push(id);
                }
            }
        }
    }

    let mut glb = Glb::default();
    let mut nodes = Vec::new();
    let mut stats = Vec::new();
    for &id in &chunks {
        let Ok(raw) = wd.chunk(id) else { continue };
        let Some(parsed) = world::mesh(&raw) else { continue };
        let mut m = mesh::chunk_mesh(&parsed);
        if m.polys.is_empty() {
            continue;
        }
        let siblings: Vec<Vec<u8>> = if id >= world::GRID_CHUNKS {
            world::interior_group(id).into_iter().filter(|&s| s != id && world::valid_chunk(s)).filter_map(|s| wd.chunk(s).ok()).collect()
        } else {
            Vec::new()
        };
        let set = wd.file(&format!("fe/{}", world::texset_for(&ids, id))).ok();
        let vram = mesh::field_vram(sys_atlas.as_deref(), set.as_deref(), &siblings, &raw);
        let atlas = mesh::build_atlas(&mut m, &vram);
        // Grid chunks sit at their overworld origin relative to the centre; interiors at 0.
        let (cx, cz) = overworld::origin(centre).unwrap_or((0, 0));
        let (ox, oz) = overworld::origin(id).map(|(x, z)| (x - cx, z - cz)).unwrap_or((0, 0));
        let (node, tris) = glb.chunk(id, &m, &atlas, ox, oz);
        nodes.push(node);
        stats.push((id, m.polys.len(), tris));
    }

    let root = glb.nodes.len();
    glb.nodes.push(json!({ "name": format!("chunk/{centre} r{radius}"), "scale": [SCALE, SCALE, SCALE], "children": nodes }));
    let bytes = glb.finish(root);
    std::fs::write(&args[2], &bytes).expect("write");
    for (id, polys, tris) in &stats {
        println!("chunk/{id}: {polys} polygons → {tris} triangles");
    }
    println!("{} chunks, {} bytes → {}", stats.len(), bytes.len(), args[2]);
}

#[derive(Default)]
struct Glb {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    meshes: Vec<Value>,
    materials: Vec<Value>,
    textures: Vec<Value>,
    images: Vec<Value>,
    nodes: Vec<Value>,
}

/// One triangle corner, ready to write.
struct Corner {
    pos: [f32; 3],
    nrm: [f32; 3],
    uv: [f32; 2],
    rgb: [f32; 3],
}

impl Glb {
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        let mut v = json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() });
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.views.push(v);
        self.views.len() - 1
    }

    fn floats(&mut self, data: &[f32], kind: &str, width: usize, bounds: bool) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let view = self.view(&bytes, Some(34962));
        let mut a = json!({ "bufferView": view, "componentType": 5126, "count": data.len() / width, "type": kind });
        if bounds {
            let mut lo = vec![f32::MAX; width];
            let mut hi = vec![f32::MIN; width];
            for c in data.chunks(width) {
                for k in 0..width {
                    lo[k] = lo[k].min(c[k]);
                    hi[k] = hi[k].max(c[k]);
                }
            }
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn material(&mut self, name: String, texture: Option<usize>, semi: bool) -> usize {
        let mut pbr = json!({ "metallicFactor": 0.0, "roughnessFactor": 1.0 });
        if let Some(t) = texture {
            pbr["baseColorTexture"] = json!({ "index": t });
        }
        if semi {
            pbr["baseColorFactor"] = json!([1.0, 1.0, 1.0, 0.5]);
        }
        let mut m = json!({ "name": name, "pbrMetallicRoughness": pbr, "doubleSided": true });
        if semi {
            m["alphaMode"] = json!("BLEND");
        } else if texture.is_some() {
            m["alphaMode"] = json!("MASK");
            m["alphaCutoff"] = json!(0.5);
        }
        self.materials.push(m);
        self.materials.len() - 1
    }

    /// One chunk as a node holding a mesh of up to four primitives. Returns (node, triangles).
    fn chunk(&mut self, id: u32, m: &mesh::RenderMesh, atlas: &mesh::Atlas, ox: i32, oz: i32) -> (usize, usize) {
        // Game space is Y down; turning 180° about X (y → −y, z → −z) gives Y up without a mirror.
        let pos = |i: u16| {
            let v = m.verts[i as usize];
            [(v[0] as i32 + ox) as f32, -(v[1] as f32), -((v[2] as i32 + oz) as f32)]
        };
        // Triangles as (poly, corners); quads are TMD "Z" order: 0-1-2 and 1-3-2.
        let mut tris: Vec<(usize, [usize; 3])> = Vec::new();
        for (k, p) in m.polys.iter().enumerate() {
            tris.push((k, [0, 1, 2]));
            if p.n == 4 {
                tris.push((k, [1, 3, 2]));
            }
        }
        // Face normals (area-weighted) and corner positions.
        let mut face: Vec<[f32; 3]> = Vec::with_capacity(tris.len());
        for &(k, c) in &tris {
            let p = &m.polys[k];
            let [a, b, d] = c.map(|i| pos(p.v[i]));
            let (u, v) = (sub(b, a), sub(d, a));
            face.push(cross(u, v));
        }
        // Smooth across corners that share a position and face within 50°.
        let key = |p: [f32; 3]| (p[0].round() as i32, p[1].round() as i32, p[2].round() as i32);
        let mut at: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
        for (t, &(k, c)) in tris.iter().enumerate() {
            for &i in &c {
                at.entry(key(pos(m.polys[k].v[i]))).or_default().push(t);
            }
        }

        let tex_w = atlas.w.max(1) as f32;
        let tex_h = atlas.h.max(1) as f32;
        // [textured opaque, untextured opaque, textured semi, untextured semi]
        let mut buckets: [Vec<Corner>; 4] = Default::default();
        for (t, &(k, c)) in tris.iter().enumerate() {
            let p = &m.polys[k];
            let n0 = norm(face[t]);
            if n0 == [0.0; 3] {
                continue; // degenerate
            }
            let b = usize::from(!p.textured()) + if p.flags & mesh::flag::SEMI != 0 { 2 } else { 0 };
            for &i in &c {
                let pp = pos(p.v[i]);
                let mut sum = [0.0f32; 3];
                for &o in &at[&key(pp)] {
                    if dot(norm(face[o]), n0) >= SMOOTH_COS {
                        sum = add(sum, face[o]);
                    }
                }
                let nrm = norm(sum);
                let [r, g, bl, _] = p.rgb[i];
                let rgb = if !p.textured() {
                    [r as f32 / 255.0, g as f32 / 255.0, bl as f32 / 255.0]
                } else if p.flags & mesh::flag::RAW != 0 {
                    [1.0; 3]
                } else {
                    [r as f32 / 128.0, g as f32 / 128.0, bl as f32 / 128.0]
                };
                // glTF vertex colours are linear; the PS1 multiplies in display space.
                let rgb = rgb.map(|c| c.powf(2.2));
                let [u, v] = p.uv[i];
                buckets[b].push(Corner { pos: pp, nrm, uv: [(u as f32 + 0.5) / tex_w, (v as f32 + 0.5) / tex_h], rgb });
            }
        }

        // The atlas: alpha 0 (texel 0x0000) is cut out, everything else opaque.
        let mut rgba = atlas.rgba.clone();
        for px in rgba.chunks_mut(4) {
            px[3] = if px[3] == 0 { 0 } else { 255 };
        }
        let png = encode_png(atlas.w, atlas.h, &rgba);
        let view = self.view(&png, None);
        self.images.push(json!({ "bufferView": view, "mimeType": "image/png", "name": format!("chunk{id}") }));
        // NEAREST both ways and clamped: PS1 texels, no mips.
        self.textures.push(json!({ "source": self.images.len() - 1, "sampler": 0 }));
        let texture = self.textures.len() - 1;

        let names = ["tex", "flat", "tex-semi", "flat-semi"];
        let mut prims = Vec::new();
        let mut count = 0;
        for (b, corners) in buckets.iter().enumerate() {
            if corners.is_empty() {
                continue;
            }
            count += corners.len() / 3;
            let textured = b % 2 == 0;
            let pos: Vec<f32> = corners.iter().flat_map(|c| c.pos).collect();
            let nrm: Vec<f32> = corners.iter().flat_map(|c| c.nrm).collect();
            let rgb: Vec<f32> = corners.iter().flat_map(|c| c.rgb).collect();
            let mut attrs = json!({
                "POSITION": self.floats(&pos, "VEC3", 3, true),
                "NORMAL": self.floats(&nrm, "VEC3", 3, false),
                "COLOR_0": self.floats(&rgb, "VEC3", 3, false),
            });
            if textured {
                let uv: Vec<f32> = corners.iter().flat_map(|c| c.uv).collect();
                attrs["TEXCOORD_0"] = json!(self.floats(&uv, "VEC2", 2, false));
            }
            let material = self.material(format!("chunk{id}-{}", names[b]), textured.then_some(texture), b >= 2);
            prims.push(json!({ "attributes": attrs, "material": material, "mode": 4 }));
        }
        self.meshes.push(json!({ "name": format!("chunk/{id}"), "primitives": prims }));
        self.nodes.push(json!({ "name": format!("chunk/{id}"), "mesh": self.meshes.len() - 1 }));
        (self.nodes.len() - 1, count)
    }

    fn finish(mut self, root: usize) -> Vec<u8> {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let doc = json!({
            "asset": { "version": "2.0", "generator": "Living Tool remaster_preview (research 16)" },
            "scene": 0,
            "scenes": [{ "nodes": [root] }],
            "nodes": self.nodes,
            "meshes": self.meshes,
            "materials": self.materials,
            "textures": self.textures,
            "images": self.images,
            "samplers": [{ "magFilter": 9728, "minFilter": 9728, "wrapS": 33071, "wrapT": 33071 }],
            "accessors": self.accessors,
            "bufferViews": self.views,
            "buffers": [{ "byteLength": self.bin.len() }],
        });
        let mut js = serde_json::to_vec(&doc).expect("json");
        while js.len() % 4 != 0 {
            js.push(b' ');
        }
        let total = 12 + 8 + js.len() + 8 + self.bin.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // "glTF"
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(js.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
        out.extend_from_slice(&js);
        out.extend_from_slice(&(self.bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
        out.extend_from_slice(&self.bin);
        out
    }
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w.max(1), h.max(1));
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().expect("png header");
        if rgba.is_empty() {
            wr.write_image_data(&[0, 0, 0, 0]).expect("png");
        } else {
            wr.write_image_data(rgba).expect("png");
        }
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l < 1e-6 {
        [0.0; 3]
    } else {
        [a[0] / l, a[1] / l, a[2] / l]
    }
}
