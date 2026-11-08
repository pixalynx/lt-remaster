//! Survey of the props baked into the retail world chunks, the research
//! behind the construction kit's stock library (`docs/research/05-props.md`).
//!
//! ```text
//! cargo run --release --example prop_survey -- <disc.bin> <out dir> repeats [interiors]
//! cargo run --release --example prop_survey -- <disc.bin> <out dir> objects <chunk>…
//! cargo run --release --example prop_survey -- <disc.bin> <out dir> stock
//! cargo run --release --example prop_survey -- <disc.bin> <out dir> furniture <chunk>…
//! ```
//!
//! - `repeats`: shapes that recur (up to translation and quarter turns),
//!   most common first, with one example each, and `repeats.png`.
//! - `objects`: every prop of a chunk with its centre cell (what a stock
//!   recipe names), and `chunkN-objects.png`, labelled by index.
//! - `stock`: every stock recipe harvested from the disc, as `stock.png`.
//! - `furniture`: the furniture welded into interior rooms, peeled off their
//!   full-height wall planes, floor and wall tops, with cells and sizes, and
//!   `chunkN-furniture.png` (research 13 §4).
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use lt_core::edit::chunk::Chunk;
use lt_core::edit::props::{self, Object};
use lt_core::raster::{draw_mesh, Canvas, Ortho, Style};
use lt_core::world::{Mesh, MeshGroup, Prim};
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

fn prims_of(c: &Chunk, o: &Object) -> Vec<Prim> {
    o.prims.iter().map(|&(g, i)| c.groups[g].prims[i]).collect()
}

fn verts(c: &Chunk, p: &Prim) -> Vec<[i16; 3]> {
    p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]).collect()
}

/// The pool seen from the south-west, 35° down: `[screen x, depth, screen up]`.
fn oblique(pool: &[[i16; 3]]) -> Vec<[i16; 3]> {
    let (s, k) = (std::f32::consts::FRAC_PI_4.sin(), std::f32::consts::FRAC_PI_4.cos());
    let e = 35f32.to_radians();
    pool.iter()
        .map(|v| {
            let (x, y, z) = (v[0] as f32, v[1] as f32, v[2] as f32);
            let (rx, rz) = (x * k - z * s, x * s + z * k);
            [rx.round() as i16, (y * e.sin() + rz * e.cos()).round() as i16, (-y * e.cos() + rz * e.sin()).round() as i16]
        })
        .collect()
}

fn mesh_of(pool: Vec<[i16; 3]>, prims: Vec<Prim>) -> Mesh {
    let n = prims.len() as u32;
    Mesh { groups: vec![MeshGroup { index: 0, mesh_ofs: 0, centre: [0; 3], lists: [0; 8], first: 0, count: n, bbox: [0; 6] }], pool, prims, mesh_end: 0 }
}

/// 3×5 digits, drawn 3× at (x, y), white on black.
fn label(cv: &mut Canvas, x: usize, y: usize, text: &str) {
    const F: [u16; 10] = [0x7B6F, 0x2492, 0x73E7, 0x73CF, 0x5BC9, 0x79CF, 0x79EF, 0x7249, 0x7BEF, 0x7BCF];
    let s = 3;
    for (k, ch) in text.chars().enumerate() {
        let Some(d) = ch.to_digit(10) else { continue };
        for py in 0..5 {
            for px in 0..3 {
                let on = F[d as usize] >> (14 - (py * 3 + px)) & 1 == 1;
                for dy in 0..s {
                    for dx in 0..s {
                        let (xx, yy) = (x + k * 4 * s + px * s + dx, y + py * s + dy);
                        if xx < cv.w && yy < cv.h {
                            let o = (yy * cv.w + xx) * 4;
                            cv.rgba[o..o + 4].copy_from_slice(&if on { [255; 4] } else { [0, 0, 0, 255] });
                        }
                    }
                }
            }
        }
    }
}

/// A sheet of `T`-px tiles, `cols` wide.
struct Sheet {
    cv: Canvas,
    cols: usize,
    t: usize,
}

impl Sheet {
    fn new(n: usize, cols: usize, t: usize) -> Self {
        Sheet { cv: Canvas::new(cols * t, n.div_ceil(cols).max(1) * t, [40, 40, 48, 255]), cols, t }
    }
    fn put(&mut self, k: usize, m: &Mesh, vram: &lt_core::vram::Vram) {
        let t = self.t - 4;
        let mut cv = Canvas::new(t, t, [24, 24, 30, 255]);
        draw_mesh(&mut cv, m, Some(vram), Ortho::fit(m, t), Style::Textured);
        label(&mut cv, 2, 2, &k.to_string());
        let (ox, oy) = ((k % self.cols) * self.t + 2, (k / self.cols) * self.t + 2);
        for y in 0..t {
            let d = ((oy + y) * self.cv.w + ox) * 4;
            self.cv.rgba[d..d + t * 4].copy_from_slice(&cv.rgba[y * t * 4..(y + 1) * t * 4]);
        }
    }
    fn save(&self, path: &std::path::Path) {
        let f = std::fs::File::create(path).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), self.cv.w as u32, self.cv.h as u32);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&self.cv.rgba).unwrap();
    }
}

/// The object's shape up to translation and quarter turns.
fn signature(c: &Chunk, comp: &[Prim]) -> (u64, [i32; 3]) {
    let mut best: Option<Vec<(u8, Vec<[i32; 3]>)>> = None;
    for r in 0..4 {
        let turn = |v: [i16; 3]| -> [i32; 3] {
            let (x, z) = (v[0] as i32, v[2] as i32);
            let (x, z) = [(x, z), (-z, x), (-x, -z), (z, -x)][r];
            [x, v[1] as i32, z]
        };
        let all: Vec<[i32; 3]> = comp.iter().flat_map(|p| verts(c, p)).map(turn).collect();
        let (mx, my, mz) = (all.iter().map(|v| v[0]).min().unwrap(), all.iter().map(|v| v[1]).max().unwrap(), all.iter().map(|v| v[2]).min().unwrap());
        let mut polys: Vec<(u8, Vec<[i32; 3]>)> = comp
            .iter()
            .map(|p| {
                let mut q: Vec<[i32; 3]> = verts(c, p).into_iter().map(turn).map(|v| [v[0] - mx, v[1] - my, v[2] - mz]).collect();
                q.sort();
                (p.list, q)
            })
            .collect();
        polys.sort();
        if best.as_ref().is_none_or(|b| polys < *b) {
            best = Some(polys);
        }
    }
    let b = best.unwrap();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    let all = b.iter().flat_map(|p| p.1.iter());
    let size = [all.clone().map(|v| v[0]).max().unwrap(), -all.clone().map(|v| v[1]).min().unwrap(), all.map(|v| v[2]).max().unwrap()];
    (h.finish(), size)
}

/// The axis-aligned upright plane a polygon lies in: (axis, x or z).
fn plane(vs: &[[i16; 3]]) -> Option<(usize, i16)> {
    [0, 2].into_iter().find(|&a| vs.iter().all(|v| v[a] == vs[0][a])).map(|a| (a, vs[0][a]))
}

/// Polygons joined by shared corners.
fn components(c: &Chunk, prims: Vec<Prim>) -> Vec<Vec<Prim>> {
    let mut parent: Vec<usize> = (0..prims.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut first: HashMap<[i16; 3], usize> = HashMap::new();
    for (k, p) in prims.iter().enumerate() {
        for v in verts(c, p) {
            let o = *first.entry(v).or_insert(k);
            let (a, b) = (find(&mut parent, k), find(&mut parent, o));
            parent[a] = b;
        }
    }
    let mut by_root: HashMap<usize, Vec<Prim>> = HashMap::new();
    for (k, p) in prims.into_iter().enumerate() {
        by_root.entry(find(&mut parent, k)).or_default().push(p);
    }
    by_root.into_values().collect()
}

fn centre_cell(o: &Object) -> (i32, i32) {
    let (x, z) = o.centre();
    ((x + 2048).div_euclid(32), (z + 2048).div_euclid(32))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&args[1]).unwrap();
    let out = std::path::PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let tex = WorldTextures::load(&wd);
    let chunk = |id: u32| {
        let raw = wd.chunk(id).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        let v = tex.vram_for(&wd, id, &raw);
        (c, v)
    };
    match args.get(3).map(String::as_str) {
        Some("objects") => {
            for id in args[4..].iter().map(|s| s.parse::<u32>().unwrap()) {
                let (c, vram) = chunk(id);
                let objs = props::objects(&c);
                let ob = oblique(&c.pool);
                let mut sheet = Sheet::new(objs.len(), 6, 192);
                for (k, o) in objs.iter().enumerate() {
                    let (cc, cr) = centre_cell(o);
                    println!(
                        "chunk {id} #{k}: {} polys · centre cell {cc},{cr} · {}×{}×{} units · x {}..{} z {}..{}",
                        o.prims.len(),
                        o.max[0] - o.min[0],
                        o.max[2] - o.min[2],
                        o.max[1] - o.min[1],
                        o.min[0],
                        o.max[0],
                        o.min[2],
                        o.max[2]
                    );
                    sheet.put(k, &mesh_of(ob.clone(), prims_of(&c, o)), &vram);
                }
                sheet.save(&out.join(format!("chunk{id}-objects.png")));
            }
        }
        Some("furniture") => {
            for id in args[4..].iter().map(|s| s.parse::<u32>().unwrap()) {
                let (c, vram) = chunk(id);
                let ob = oblique(&c.pool);
                let mut found: Vec<(usize, Vec<Prim>, [i16; 3], [i16; 3])> = Vec::new();
                for (r, room) in props::objects(&c).iter().enumerate().filter(|(_, o)| o.prims.len() >= 150 && o.max[1] - o.min[1] >= 160) {
                    let prims = prims_of(&c, room);
                    let mut planes: HashMap<(usize, i16), (i16, i16)> = HashMap::new();
                    for p in &prims {
                        let vs = verts(&c, p);
                        if let Some(key) = plane(&vs) {
                            let e = planes.entry(key).or_insert((i16::MAX, i16::MIN));
                            for v in &vs {
                                *e = (e.0.min(v[1]), e.1.max(v[1]));
                            }
                        }
                    }
                    let (top, floor) = (room.min[1], room.max[1]);
                    let keep: Vec<Prim> = prims
                        .into_iter()
                        .filter(|p| {
                            let vs = verts(&c, p);
                            let wall = plane(&vs).is_some_and(|k| planes[&k].1 - planes[&k].0 >= floor - top - 8);
                            let level = vs.iter().all(|v| v[1] == vs[0][1]) && (vs[0][1] == top || vs[0][1] == floor);
                            !wall && !level
                        })
                        .collect();
                    for comp in components(&c, keep) {
                        let vs: Vec<[i16; 3]> = comp.iter().flat_map(|p| verts(&c, p)).collect();
                        let lo = [0, 1, 2].map(|a| vs.iter().map(|v| v[a]).min().unwrap());
                        let hi = [0, 1, 2].map(|a| vs.iter().map(|v| v[a]).max().unwrap());
                        if comp.len() >= 4 {
                            found.push((r, comp, lo, hi));
                        }
                    }
                }
                found.sort_by_key(|f| std::cmp::Reverse(f.1.len()));
                let mut sheet = Sheet::new(found.len(), 6, 192);
                for (k, (r, comp, lo, hi)) in found.iter().enumerate() {
                    let (cx, cz) = ((lo[0] as i32 + hi[0] as i32) / 2, (lo[2] as i32 + hi[2] as i32) / 2);
                    println!(
                        "chunk {id} room #{r} piece {k}: {} polys · centre cell {},{} · {}×{}×{} units · cells {}..{} × {}..{}",
                        comp.len(),
                        (cx + 2048).div_euclid(32),
                        (cz + 2048).div_euclid(32),
                        hi[0] - lo[0],
                        hi[2] - lo[2],
                        hi[1] - lo[1],
                        (lo[0] as i32 + 2048).div_euclid(32),
                        (hi[0] as i32 + 2047).div_euclid(32),
                        (lo[2] as i32 + 2048).div_euclid(32),
                        (hi[2] as i32 + 2047).div_euclid(32),
                    );
                    sheet.put(k, &mesh_of(ob.clone(), comp.clone()), &vram);
                }
                sheet.save(&out.join(format!("chunk{id}-furniture.png")));
            }
        }
        Some("stock") => {
            use lt_core::edit::{house, stock};
            let items: Vec<stock::Item> = stock::catalogue().into_iter().filter(|i| i.roof.is_none_or(|r| r == "blue")).collect();
            let specs = [
                house::HouseSpec { roof: house::Roof::Orange, length: 3, door: Some(1), windows: true, chimney: Some(0) },
                house::HouseSpec { roof: house::Roof::Red, length: 1, door: Some(0), windows: false, chimney: None },
                house::HouseSpec { roof: house::Roof::Blue, length: 5, door: Some(2), windows: true, chimney: Some(4) },
            ];
            let mut sheet = Sheet::new(items.len() + specs.len(), 8, 200);
            let show = |k: usize, label: &str, p: lt_core::Result<lt_core::edit::prefab::Prefab>, sheet: &mut Sheet| {
                let p = match p {
                    Ok(p) => p,
                    Err(e) => return println!("{k:3} {label:24} FAILED {e}"),
                };
                println!("{k:3} {label:24} {:>4} polys · {:2}×{:2} cells · {:3} high · {} tiles", p.prims.len(), p.area.w(), p.area.h(), p.height(), p.tiles.len());
                let mut b = Chunk::blank(0);
                b.texture.image.h = 512;
                b.texture.pixels = vec![0; 64 * 512 * 2];
                if let Some((rc, e)) = &mut b.texture.clut {
                    rc.h = 32;
                    *e = vec![0; 16 * 32];
                }
                let at = lt_core::edit::prefab::Placement { col: 0, row: 0, rot: 0, ground_y: Some(0), write_ground: false, lift: 0 };
                if let Err(e) = lt_core::edit::prefab::place(&mut b, &p, at) {
                    return println!("    place: {}", e.message);
                }
                let bytes = b.serialize().unwrap();
                let m = lt_core::world::mesh(&bytes).unwrap();
                let mut v = lt_core::vram::Vram::new();
                v.paint_tim(&bytes[lt_core::world::TIM_OFFSET..]);
                sheet.put(k, &mesh_of(oblique(&m.pool), m.prims.clone()), &v);
            };
            for (k, it) in items.iter().enumerate() {
                let (c, vram) = chunk(it.source.chunk());
                let p = stock::make(it, &c, Some(&vram)).map_err(|e| lt_core::LtError::FileNotFound(e.message));
                show(k, &it.id, p, &mut sheet);
            }
            let (c78, _) = chunk(78);
            for (j, spec) in specs.iter().enumerate() {
                let p = house::house(*spec, &c78.texture).map_err(|e| lt_core::LtError::FileNotFound(e.message));
                show(items.len() + j, "house", p, &mut sheet);
            }
            sheet.save(&out.join("stock.png"));
        }
        _ => {
            let interiors = args.get(4).is_some_and(|a| a == "interiors");
            struct Seen {
                count: usize,
                chunks: Vec<u32>,
                first: (u32, usize),
                polys: usize,
                size: [i32; 3],
            }
            let mut seen: HashMap<u64, Seen> = HashMap::new();
            let mut order: Vec<u64> = Vec::new();
            for id in 0..524 {
                let Ok(raw) = wd.chunk(id) else { continue };
                let Ok(c) = Chunk::parse(&raw) else { continue };
                for (k, o) in props::objects(&c).iter().enumerate() {
                    if o.prims.len() < 3 {
                        continue;
                    }
                    let (h, size) = signature(&c, &prims_of(&c, o));
                    let e = seen.entry(h).or_insert_with(|| {
                        order.push(h);
                        Seen { count: 0, chunks: Vec::new(), first: (id, k), polys: o.prims.len(), size }
                    });
                    e.count += 1;
                    if !e.chunks.contains(&id) {
                        e.chunks.push(id);
                    }
                }
            }
            let mut rep: Vec<u64> = order.into_iter().filter(|h| seen[h].count >= 3 && (seen[h].first.0 >= 324) == interiors && seen[h].size[0] < 1024).collect();
            rep.sort_by_key(|h| std::cmp::Reverse(seen[h].count));
            println!("{} distinct shapes, {} seen 3+ times", seen.len(), rep.len());
            let n = rep.len().min(64);
            let mut sheet = Sheet::new(n, 8, 200);
            for (i, h) in rep.iter().take(n).enumerate() {
                let s = &seen[h];
                let (id, k) = s.first;
                let (c, vram) = chunk(id);
                let o = &props::objects(&c)[k];
                let (cc, cr) = centre_cell(o);
                println!(
                    "R{i:03} ×{:4} in {:3} chunks · {:4} polys · {}×{}×{} units · chunk {id} centre cell {cc},{cr} · chunks {:?}",
                    s.count,
                    s.chunks.len(),
                    s.polys,
                    s.size[0],
                    s.size[2],
                    s.size[1],
                    &s.chunks[..s.chunks.len().min(10)]
                );
                sheet.put(i, &mesh_of(oblique(&c.pool), prims_of(&c, o)), &vram);
            }
            sheet.save(&out.join("repeats.png"));
        }
    }
}
