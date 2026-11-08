//! Survey behind the house workbench's styles (screen 23 phase 4,
//! `docs/research/14-house-styles.md`): the retail buildings of every town,
//! measured the way `edit::design` builds a log house.
//!
//! ```text
//! cargo run --release --example style_survey -- <disc.bin> <out dir> [chunk…]
//! cargo run --release --example style_survey -- <disc.bin> <out dir> detail <chunk> <piece>
//! ```
//!
//! `detail` lists one building's polygons (heights above its ground, plan
//! corners from its min corner, texels and CLUT, the way it faces) and writes
//! its chunk's page through each CLUT the building uses (`cN-clut-XXXX.png`).
//!
//! Without chunks it sweeps every chunk. A building is an unbake piece
//! (`edit::unbake::split`) at least 160 units on a side and 150 high. For each
//! one it prints its size, the wall faces' widths and the heights they stop
//! at, the roof's pitch and which ways its slopes face (four ways and no
//! gable = a hip roof), and the texture rectangles and CLUTs its walls and
//! roof use, and writes `cN-pK.png`, a 3/4 thumbnail.
use std::collections::BTreeMap;

use lt_core::edit::chunk::Chunk;
use lt_core::edit::prefab::{self, CellRect, HarvestOptions};
use lt_core::edit::unbake;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn normal(v: &[[i16; 3]]) -> [f32; 3] {
    // Newell's method; pool is Y down, so flip y to get "up".
    let mut n = [0f32; 3];
    for i in 0..v.len() {
        let (a, b) = (v[i], v[(i + 1) % v.len()]);
        let (a, b) = ([a[0] as f32, -a[1] as f32, a[2] as f32], [b[0] as f32, -b[1] as f32, b[2] as f32]);
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
    n.map(|c| c / l)
}

fn bump<K: Ord>(m: &mut BTreeMap<K, usize>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

fn top<K: Ord + std::fmt::Debug + Clone>(m: &BTreeMap<K, usize>, n: usize) -> String {
    let mut v: Vec<(K, usize)> = m.iter().map(|(k, c)| (k.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.iter().take(n).map(|(k, c)| format!("{k:?}×{c}")).collect::<Vec<_>>().join(" ")
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let out = &a[2];
    std::fs::create_dir_all(out).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    if a.get(3).map(String::as_str) == Some("detail") {
        let (id, k): (u32, usize) = (a[4].parse().unwrap(), a[5].parse().unwrap());
        let c = Chunk::parse(&wd.chunk(id).unwrap()).unwrap();
        detail(&c, id, &unbake::split(&c).pieces[k], out);
        return;
    }
    let chunks: Vec<u32> = if a.len() > 3 { a[3..].iter().map(|s| s.parse().unwrap()).collect() } else { (0..1328).collect() };
    for id in chunks {
        let Ok(bytes) = wd.chunk(id) else { continue };
        let Ok(c) = Chunk::parse(&bytes) else { continue };
        let split = unbake::split(&c);
        for (k, p) in split.pieces.iter().enumerate() {
            let (sx, sz, sy) = (p.max[0] - p.min[0], p.max[2] - p.min[2], p.max[1] - p.min[1]);
            if sx < 160 || sz < 160 || sy < 150 || p.prims.len() < 24 {
                continue;
            }
            let ground = p.max[1];
            let mut widths = BTreeMap::new();
            let mut wall_tops = BTreeMap::new();
            let mut facing = BTreeMap::new();
            let mut pitch = BTreeMap::new();
            let mut wall_tex = BTreeMap::new();
            let mut roof_tex = BTreeMap::new();
            let mut upright_tris_high = 0;
            let mut pages = BTreeMap::new();
            for &(g, i) in &p.prims {
                let q = &c.groups[g].prims[i];
                let v: Vec<[i16; 3]> = q.idx[..q.n as usize].iter().map(|&j| c.pool[j as usize]).collect();
                let n = normal(&v);
                let (lo, hi) = (v.iter().map(|w| w[1]).max().unwrap(), v.iter().map(|w| w[1]).min().unwrap());
                let (h_lo, h_hi) = (ground - lo, ground - hi);
                let uv = |q: &lt_core::world::Prim| {
                    let us = q.uv[..q.n as usize].iter().map(|t| t[0]);
                    let vs = q.uv[..q.n as usize].iter().map(|t| t[1]);
                    [us.clone().min().unwrap(), vs.clone().min().unwrap(), us.max().unwrap(), vs.max().unwrap()]
                };
                if q.textured() {
                    bump(&mut pages, q.tpage & 0x1F);
                }
                if n[1].abs() < 0.2 {
                    // Upright: a wall, a gable or a chimney side.
                    let xs = v.iter().map(|w| w[0]);
                    let zs = v.iter().map(|w| w[2]);
                    let w = (xs.clone().max().unwrap() - xs.min().unwrap()).max(zs.clone().max().unwrap() - zs.min().unwrap());
                    bump(&mut widths, w);
                    bump(&mut wall_tops, h_hi);
                    if q.n == 3 && h_lo >= 120 {
                        upright_tris_high += 1;
                    }
                    if q.textured() {
                        bump(&mut wall_tex, (uv(q), q.clut));
                    }
                } else if n[1] > 0.2 && h_lo >= 100 {
                    // A roof slope: its pitch as rise per 128 and the way it faces.
                    let run = (n[0] * n[0] + n[2] * n[2]).sqrt();
                    bump(&mut pitch, ((run / n[1]) * 128.0).round() as i32);
                    let ang = n[2].atan2(n[0]).to_degrees();
                    let dir = ((ang + 360.0 + 22.5) % 360.0 / 45.0) as usize;
                    bump(&mut facing, ["E", "NE", "N", "NW", "W", "SW", "S", "SE"][dir]);
                    if q.textured() {
                        bump(&mut roof_tex, (uv(q), q.clut));
                    }
                }
            }
            println!(
                "chunk {id:>4} piece {k:>3}: {sx:>4}×{sz:<4} h {sy:>3} · {:>4} polys · pages {} · page tex {}×{}@{},{}",
                p.prims.len(),
                top(&pages, 3),
                c.texture.image.w,
                c.texture.image.h,
                c.texture.image.x,
                c.texture.image.y
            );
            println!("    widths {}", top(&widths, 6));
            println!("    upright tops {}", top(&wall_tops, 8));
            println!("    roof pitch/128 {} · faces {} · high upright tris {upright_tris_high}", top(&pitch, 4), top(&facing, 8));
            println!("    wall tex {}", top(&wall_tex, 5));
            println!("    roof tex {}", top(&roof_tex, 4));
            let area = CellRect { c0: 0, r0: 0, c1: 0, r1: 0 };
            let opts = HarvestOptions { prims: p.prims.clone(), ..Default::default() };
            if let Ok(pf) = prefab::harvest(&c, id, area, &format!("c{id}-p{k}"), opts) {
                if let Some(png) = prefab::thumbnail(&pf, 320) {
                    std::fs::write(format!("{out}/c{id}-p{k}.png"), png).unwrap();
                }
            }
        }
    }
}

fn detail(c: &Chunk, id: u32, p: &unbake::Piece, out: &str) {
    let ground = p.max[1];
    let mut cluts = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for &(g, i) in &p.prims {
        let q = &c.groups[g].prims[i];
        let v: Vec<[i16; 3]> = q.idx[..q.n as usize].iter().map(|&j| c.pool[j as usize]).collect();
        let n = normal(&v);
        let kind = if n[1].abs() < 0.2 {
            let ang = n[2].atan2(n[0]).to_degrees();
            ["E", "NE", "N", "NW", "W", "SW", "S", "SE"][((ang + 360.0 + 22.5) % 360.0 / 45.0) as usize]
        } else if n[1] > 0.0 {
            "up"
        } else {
            "down"
        };
        let pts: Vec<String> = v.iter().map(|w| format!("({},{},{})", w[0] - p.min[0], ground - w[1], w[2] - p.min[2])).collect();
        let uvs: Vec<String> = q.uv[..q.n as usize].iter().map(|t| format!("{},{}", t[0], t[1])).collect();
        if q.textured() {
            cluts.insert(q.clut);
        }
        let lo = v.iter().map(|w| ground - w[1]).min().unwrap();
        rows.push((lo, format!("{kind:>4} l{} {} tp{:>2} clut {:#06x} rgb {:?} | {} | uv {}", q.list, q.n, q.tpage & 0x1F, q.clut, q.rgb[0], pts.join(" "), uvs.join(" "))));
    }
    rows.sort_by_key(|r| r.0);
    if std::env::var("ROWS").is_ok() {
        for (_, r) in rows {
            println!("{r}");
        }
    }
    // Elements: polygons grouped by (page, CLUT, texel box), with the units they span.
    let mut el: BTreeMap<(u16, u16, [u8; 4]), (usize, &str, i16, i16, BTreeMap<(i16, i16), usize>)> = BTreeMap::new();
    for &(g, i) in &p.prims {
        let q = &c.groups[g].prims[i];
        if !q.textured() {
            continue;
        }
        let v: Vec<[i16; 3]> = q.idx[..q.n as usize].iter().map(|&j| c.pool[j as usize]).collect();
        let n = normal(&v);
        let kind = if n[1].abs() < 0.2 { "upright" } else if n[1] > 0.0 { "slope/flat up" } else { "down" };
        let us = q.uv[..q.n as usize].iter().map(|t| t[0]);
        let vs = q.uv[..q.n as usize].iter().map(|t| t[1]);
        let r = [us.clone().min().unwrap(), vs.clone().min().unwrap(), us.max().unwrap(), vs.max().unwrap()];
        let hs = v.iter().map(|w| ground - w[1]);
        let (lo, hi) = (hs.clone().min().unwrap(), hs.max().unwrap());
        let xs = v.iter().map(|w| w[0]);
        let zs = v.iter().map(|w| w[2]);
        let span = (xs.clone().max().unwrap() - xs.min().unwrap()).max(zs.clone().max().unwrap() - zs.min().unwrap());
        let e = el.entry((q.tpage & 0x1F, q.clut, r)).or_insert((0, kind, i16::MAX, i16::MIN, BTreeMap::new()));
        e.0 += 1;
        e.2 = e.2.min(lo);
        e.3 = e.3.max(hi);
        bump(&mut e.4, (span, hi - lo));
    }
    let mut els: Vec<_> = el.into_iter().collect();
    els.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let mut rects = String::new();
    for (k, ((tp, clut, r), (n, kind, lo, hi, sizes))) in els.iter().enumerate() {
        println!("#{k:<3} tp{tp:>2} clut {clut:#06x} tex {:>3},{:>3}–{:>3},{:>3} ({:>3}×{:<3}) ×{n:<3} {kind:<13} h {lo:>3}–{hi:<3} spans {}", r[0], r[1], r[2], r[3], r[2] as i32 - r[0] as i32 + 1, r[3] as i32 - r[1] as i32 + 1, top(sizes, 3));
        rects.push_str(&format!("{k} {clut} {} {} {} {}\n", r[0], r[1], r[2], r[3]));
    }
    std::fs::write(format!("{out}/c{id}-rects.txt"), rects).unwrap();
    let t = &c.texture;
    let Some((rect, entries)) = t.clut.as_ref() else { return };
    for clut in cluts {
        let (row, col) = (((clut >> 6) as i32) - rect.y as i32, ((clut & 0x3F) as i32) * 16 - rect.x as i32);
        if row < 0 || col < 0 || row >= rect.h as i32 || col + 16 > rect.w as i32 {
            continue;
        }
        let pal: Vec<[u8; 4]> = (0..16)
            .map(|i| {
                let e = entries[row as usize * rect.w as usize + col as usize + i];
                let f = |s: u16| ((e >> s & 31) * 255 / 31) as u8;
                [f(0), f(5), f(10), if e == 0 { 0 } else { 255 }]
            })
            .collect();
        let (w, h) = (t.image.w as usize * 4, t.image.h as usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let b = t.pixels[y * (w / 2) + x / 2];
                rgba.extend_from_slice(&pal[if x & 1 == 0 { b & 15 } else { b >> 4 } as usize]);
            }
        }
        let mut png_out = Vec::new();
        let mut enc = png::Encoder::new(&mut png_out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&rgba).unwrap();
        wr.finish().unwrap();
        std::fs::write(format!("{out}/c{id}-clut-{clut:04x}.png"), png_out).unwrap();
    }
}
