//! Survey for unbaking retail chunks (`edit::unbake`, `docs/research/12-unbake.md`).
//!
//! ```text
//! cargo run --release --example unbake_survey -- <disc.bin> <out dir> <chunk>…
//! cargo run --release --example unbake_survey -- <disc.bin> <out dir> sweep
//! ```
//!
//! - `<chunk>…`: the chunk's pieces next to [`props::objects`] (props joined
//!   where they share a vertex), what moving each would touch (walk cells that
//!   aren't the terrain's own bake, vertices shared with the terrain, nearby
//!   door actors, examine spots, smoke and actors of the chunk's pack), the
//!   terrain's brightness by distance to the nearest building (baked shadows?),
//!   and `chunkN-split.png` (top view, one colour per piece, terrain grey,
//!   ground decals pale), `chunkN-split-sheet.png` (every piece from the
//!   south-west) and `chunkN-light.png` (vertex colours alone).
//! - `sweep`: every movable piece of every chunk deleted, and moved 2 cells
//!   east: how many rebakes fit the chunk's budgets, how many holes find
//!   ground to fill with, and the timings.
use std::collections::{BTreeSet, HashSet};

use lt_core::edit::bake;
use lt_core::edit::chunk::Chunk;
use lt_core::edit::props;
use lt_core::edit::unbake::{self, Kind, Move};
use lt_core::npc::NpcPack;
use lt_core::raster::{draw_mesh, Canvas, Ortho, Style};
use lt_core::trigger::{Outcome, TriggerRecord};
use lt_core::world::{Mesh, MeshGroup, Prim};
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

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

fn flat(p: &Prim, rgb: [u8; 3]) -> Prim {
    let mut q = *p;
    q.list = if q.n == 4 { 2 } else { 0 };
    q.code &= !3;
    q.rgb = [rgb; 4];
    q
}

fn palette(n: usize) -> [u8; 3] {
    let h = (n as f32 * 0.618_034).fract() * 6.0;
    let (r, g, b) = match h as u32 {
        0 => (1.0, h.fract(), 0.0),
        1 => (1.0 - h.fract(), 1.0, 0.0),
        2 => (0.0, 1.0, h.fract()),
        3 => (0.0, 1.0 - h.fract(), 1.0),
        4 => (h.fract(), 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - h.fract()),
    };
    let m = |v: f32| (60.0 + v * 170.0) as u8;
    [m(r), m(g), m(b)]
}

fn save(cv: &Canvas, path: &std::path::Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), cv.w as u32, cv.h as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&cv.rgba).unwrap();
}

/// 3×5 digits at (x, y), `s`× scale, white on black.
fn label(cv: &mut Canvas, x: usize, y: usize, text: &str, s: usize) {
    const F: [u16; 10] = [0x7B6F, 0x2492, 0x73E7, 0x73CF, 0x5BC9, 0x79CF, 0x79EF, 0x7249, 0x7BEF, 0x7BCF];
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

/// Things the chunk's pack places by position: what a move must carry.
fn markers(w: &WorldDisc, table: &[(u16, u16)], id: u32) -> Vec<(String, i32, i32)> {
    let mut out = Vec::new();
    let Some(pack) = lt_core::world::pack_for(table, id, &|_| false) else { return out };
    let Ok(b) = w.file(&format!("npc/{pack}")) else { return out };
    let Some(p) = NpcPack::parse(&b) else { return out };
    for g in p.triggers(&b).iter().filter(|g| g.chunk as u32 == id) {
        for r in &g.rungs {
            for rec in &r.records {
                let (what, x, z) = match rec {
                    TriggerRecord::Door { x, z, to, .. } => (format!("door→{to}"), *x, *z),
                    TriggerRecord::Set { x, z, .. } | TriggerRecord::Clear { x, z, .. } => ("flag".into(), *x, *z),
                    TriggerRecord::Talk { x, z, actor, .. } => (format!("scene {actor}"), *x, *z),
                    TriggerRecord::Other { x, z, .. } => ("other".into(), *x, *z),
                };
                out.push((what, x as i32, z as i32));
            }
        }
    }
    for (slot, a) in p.actors.iter().enumerate().skip(2) {
        if (a.b & 0xFFFF) as u32 != id {
            continue;
        }
        let what = match (a.d >> 8) & 0xFF {
            2 => "door actor",
            4 => "examine",
            5 => "smoke",
            _ => "actor",
        };
        if let Some(Outcome::Place { x, z, .. }) = p.placement(&b, slot).into_iter().map(|br| br.outcome).find(|o| matches!(o, Outcome::Place { .. })) {
            out.push((format!("{what} {slot}"), x as i32, z as i32));
        }
    }
    out
}

fn terrain_only(c: &Chunk) -> Chunk {
    let mut t = c.clone();
    for g in t.groups.iter_mut() {
        g.prims.retain(|p| props::is_terrain(c, p));
    }
    t
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&args[1]).unwrap();
    let out = std::path::PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    if args.get(3).is_some_and(|a| a == "sweep") {
        return sweep(&wd);
    }
    let tex = WorldTextures::load(&wd);
    let packs = wd.file("ovr/2").map(|o| lt_core::world::pack_table_in(&exe, &o)).unwrap_or_default();
    for id in args[3..].iter().map(|s| s.parse::<u32>().unwrap()) {
        let raw = wd.chunk(id).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        let kinds = unbake::classify(&c);
        let split = unbake::split(&c);
        let old = props::objects(&c);
        let tbake = bake::bake(&terrain_only(&c));
        let marks = markers(&wd, &packs, id);
        println!(
            "chunk {id}: {} polygons {:?} · {} groups · vertex join {} objects (largest {}) → {} pieces (largest {}, {} fixed), {} decals stay as ground",
            kinds.len(),
            split.kinds,
            c.groups.len(),
            old.len(),
            old.first().map_or(0, |o| o.prims.len()),
            split.pieces.len(),
            split.pieces.first().map_or(0, |p| p.prims.len()),
            split.pieces.iter().filter(|p| p.fixed).count(),
            split.ground.len()
        );
        let corners = |&(g, i): &(usize, usize)| {
            let q = &c.groups[g].prims[i];
            q.idx[..q.n as usize].to_vec()
        };
        let terrain_idx: HashSet<u16> = kinds.iter().filter(|(_, k)| **k == Kind::Terrain).flat_map(|(key, _)| corners(key)).collect();
        for (n, p) in split.pieces.iter().enumerate() {
            let cells = unbake::cells_under(&c, &p.prims);
            let collide = cells.iter().filter(|&&k| c.heights[k] != tbake[k]).count();
            let idx: HashSet<u16> = p.prims.iter().flat_map(corners).collect();
            let shared = idx.iter().filter(|i| terrain_idx.contains(i)).count();
            let groups: BTreeSet<usize> = p.prims.iter().map(|k| k.0).collect();
            let near: Vec<&str> = marks
                .iter()
                .filter(|(_, x, z)| *x >= p.min[0] as i32 - 64 && *x <= p.max[0] as i32 + 64 && *z >= p.min[2] as i32 - 64 && *z <= p.max[2] as i32 + 64)
                .map(|m| m.0.as_str())
                .collect();
            println!(
                "  #{n:<3} {:>4} polygons ({} decals) · {}×{}×{} at x {}..{} z {}..{} · {} cells, {} walk cells not the terrain's · {} vertices shared with terrain · {} groups{}{}",
                p.prims.len(),
                p.decals,
                p.max[0] - p.min[0],
                p.max[2] - p.min[2],
                p.max[1] - p.min[1],
                p.min[0],
                p.max[0],
                p.min[2],
                p.max[2],
                cells.len(),
                collide,
                shared,
                groups.len(),
                if p.fixed { " · FIXED" } else { "" },
                if near.is_empty() { String::new() } else { format!(" · near: {}", near.join(", ")) }
            );
        }
        // Baked shadows? Terrain brightness by distance to the nearest building.
        let big: Vec<&unbake::Piece> = split.pieces.iter().filter(|p| p.prims.len() >= 50 && !p.fixed).collect();
        let mut bins = [(0u64, 0u64); 8];
        for (&(g, i), k) in &kinds {
            if *k != Kind::Terrain {
                continue;
            }
            let p = &c.groups[g].prims[i];
            for j in 0..p.n as usize {
                let v = c.pool[p.idx[j] as usize];
                let d = big
                    .iter()
                    .map(|o| {
                        let dx = (o.min[0] as i32 - v[0] as i32).max(v[0] as i32 - o.max[0] as i32).max(0);
                        let dz = (o.min[2] as i32 - v[2] as i32).max(v[2] as i32 - o.max[2] as i32).max(0);
                        dx.max(dz)
                    })
                    .min()
                    .unwrap_or(9999);
                let b = ((d / 64) as usize).min(7);
                bins[b].0 += p.rgb[j][0] as u64;
                bins[b].1 += 1;
            }
        }
        println!("  terrain brightness by distance to a building (64-unit bins): {}", bins.iter().map(|(s, n)| if *n > 0 { format!("{}({n})", s / n) } else { "-".into() }).collect::<Vec<_>>().join(" "));
        // Top view: terrain grey, ground decals pale, pieces coloured.
        let size = 1024;
        let mut prims = Vec::new();
        for (&(g, i), k) in &kinds {
            let p = &c.groups[g].prims[i];
            match k {
                Kind::Terrain => prims.push(flat(p, [70, 74, 80])),
                Kind::Decal => prims.push(flat(p, [150, 150, 140])),
                Kind::Prop => {}
            }
        }
        for (n, p) in split.pieces.iter().enumerate() {
            prims.extend(p.prims.iter().map(|&(g, i)| flat(&c.groups[g].prims[i], palette(n))));
        }
        let mut cv = Canvas::new(size, size, [20, 20, 26, 255]);
        draw_mesh(&mut cv, &mesh_of(c.pool.clone(), prims), None, Ortho::chunk(size), Style::Textured);
        let o = Ortho::chunk(size);
        for (n, p) in split.pieces.iter().enumerate().take(80) {
            let (x, z) = p.centre();
            let (px, py) = (((x as f32 - o.left) * o.scale) as usize, ((o.top - z as f32) * o.scale) as usize);
            label(&mut cv, px.saturating_sub(6), py.saturating_sub(5), &n.to_string(), 2);
        }
        save(&cv, &out.join(format!("chunk{id}-split.png")));
        // Every piece from the south-west, textured.
        let vram = tex.vram_for(&wd, id, &raw);
        let ob = oblique(&c.pool);
        let (cols, t) = (6usize, 192usize);
        let n = split.pieces.len().min(36);
        let mut sheet = Canvas::new(cols * t, n.div_ceil(cols).max(1) * t, [40, 40, 48, 255]);
        for (k, p) in split.pieces.iter().take(n).enumerate() {
            let tt = t - 4;
            let mut cv = Canvas::new(tt, tt, [24, 24, 30, 255]);
            let m = mesh_of(ob.clone(), p.prims.iter().map(|&(g, i)| c.groups[g].prims[i]).collect());
            draw_mesh(&mut cv, &m, Some(&vram), Ortho::fit(&m, tt), Style::Textured);
            label(&mut cv, 2, 2, &k.to_string(), 3);
            let (ox, oy) = ((k % cols) * t + 2, (k / cols) * t + 2);
            for y in 0..tt {
                let d = ((oy + y) * sheet.w + ox) * 4;
                sheet.rgba[d..d + tt * 4].copy_from_slice(&cv.rgba[y * tt * 4..(y + 1) * tt * 4]);
            }
        }
        save(&sheet, &out.join(format!("chunk{id}-split-sheet.png")));
        // Vertex colours alone (untextured gouraud).
        let lit: Vec<Prim> = c
            .groups
            .iter()
            .flat_map(|g| g.prims.iter())
            .map(|p| {
                let mut q = *p;
                q.list = if q.n == 4 { 6 } else { 4 };
                q.code &= !3;
                if !p.gouraud() {
                    q.rgb = [p.rgb[0]; 4];
                }
                q
            })
            .collect();
        let mut cv = Canvas::new(size, size, [20, 20, 26, 255]);
        draw_mesh(&mut cv, &mesh_of(c.pool.clone(), lit), None, Ortho::chunk(size), Style::Textured);
        save(&cv, &out.join(format!("chunk{id}-light.png")));
    }
}

fn sweep(wd: &WorldDisc) {
    let t0 = std::time::Instant::now();
    let (mut chunks, mut pieces, mut fixed, mut old_objects) = (0usize, 0usize, 0usize, 0usize);
    let (mut del_ok, mut mov_ok, mut tried) = (0usize, 0usize, 0usize);
    let (mut holes, mut filled) = (0usize, 0usize);
    let mut errors: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut slowest = (0u128, 0u32);
    let (mut tint, mut neutral) = (0usize, 0usize);
    let mut lost_by: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut hole_chunks: BTreeSet<u32> = BTreeSet::new();
    for id in 0..524 {
        let Ok(raw) = wd.chunk(id) else { continue };
        let Ok(c) = Chunk::parse(&raw) else { continue };
        chunks += 1;
        old_objects += props::objects(&c).len();
        for g in &c.groups {
            for p in &g.prims {
                if props::is_terrain(&c, p) {
                    for q in &p.rgb[..p.n as usize] {
                        if *q == [128, 128, 128] {
                            neutral += 1;
                        } else {
                            tint += 1;
                        }
                    }
                }
            }
        }
        let t = std::time::Instant::now();
        let split = unbake::split(&c);
        let before = bake::surfaces(&c);
        let ms = t.elapsed().as_millis();
        if ms > slowest.0 {
            slowest = (ms, id);
        }
        pieces += split.pieces.len();
        for (n, p) in split.pieces.iter().enumerate() {
            if p.fixed {
                fixed += 1;
                continue;
            }
            tried += 1;
            match unbake::rebake(&c, &split, &[Move { piece: n, dc: 0, dr: 0, turns: 0, delete: true, ..Default::default() }]) {
                Ok((out, rep)) => {
                    del_ok += 1;
                    // Holes left: cells the piece covered that have no floor now.
                    let left = unbake::cells_under(&c, &p.prims);
                    let s = bake::surfaces(&out);
                    let floor = |s: &[Vec<bake::Surface>], k: usize| s[k].iter().any(|f| f.flat >= bake::FLOOR_FLAT);
                    let lost = left.iter().filter(|&&k| floor(&before, k) && !floor(&s, k)).count();
                    holes += lost;
                    filled += rep.filled_cells;
                    if lost > 0 {
                        *lost_by.entry(if id >= 324 { "interior" } else { "overworld" }).or_default() += lost;
                        hole_chunks.insert(id);
                    }
                }
                Err(e) => *errors.entry(format!("delete {}", e.code)).or_default() += 1,
            }
            let dc = if p.max[0] as i32 + 64 < 2048 { 2 } else { -2 };
            match unbake::rebake(&c, &split, &[Move { piece: n, dc, dr: 0, turns: 0, delete: false, ..Default::default() }]) {
                Ok((out, _)) => match out.serialize() {
                    Ok(_) => mov_ok += 1,
                    Err(e) => *errors.entry(format!("move {}", e.code)).or_default() += 1,
                },
                Err(e) => *errors.entry(format!("move {}", e.code)).or_default() += 1,
            }
        }
    }
    println!("{chunks} chunks · vertex join {old_objects} objects → {pieces} pieces ({fixed} fixed at a chunk edge)");
    println!("movable pieces {tried}: deleted ok {del_ok}, moved 2 cells and serialized ok {mov_ok}");
    println!("cells that had a floor and have none after a delete: {holes} {lost_by:?} in {} chunks (cells filled: {filled}); first chunks {:?}", hole_chunks.len(), hole_chunks.iter().take(20).collect::<Vec<_>>());
    println!("errors {errors:?}");
    println!("terrain vertex colours: {neutral} neutral, {tint} tinted");
    println!("slowest split {} ms (chunk {}); whole sweep {:.1} s", slowest.0, slowest.1, t0.elapsed().as_secs_f32());
}
