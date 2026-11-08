//! Survey behind research 13 (house interiors, for the house workbench,
//! screen 23): where the village houses' doors lead, what the rooms behind
//! them are made of, and which retail furniture a kit could reuse.
//!
//! ```text
//! cargo run --release --example interior_survey -- <disc.bin> doors [chunk]
//! cargo run --release --example interior_survey -- <disc.bin> rooms <out dir> <chunk>…
//! cargo run --release --example interior_survey -- <disc.bin> pairs
//! ```
//!
//! - `doors`: every actor homed on `chunk` (default 78) whose program warps, with
//!   where it stands and where it leads.
//! - `pairs`: each of Orgo's house doors with the house it's in and the room it
//!   opens into, for the inside scale.
//! - `rooms`: per chunk, the walkable regions, the polygons by texture (page,
//!   palette and UV box) split into floor, wall and other, and a contact sheet of
//!   those textures as `<out>/chunkN-tex.png`.
use std::collections::{BTreeMap, VecDeque};

use lt_core::edit::chunk::Chunk;
use lt_core::edit::unbake;
use lt_core::npc::NpcPack;
use lt_core::tier2;
use lt_core::trigger::{placement_ladder, Outcome};
use lt_core::vram::{c5, Texel};
use lt_core::world::Prim;
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    match a[2].as_str() {
        "doors" => doors(&wd, a.get(3).map(|s| s.parse().unwrap()).unwrap_or(78)),
        "rooms" => {
            let tex = WorldTextures::load(&wd);
            for id in a[4..].iter().map(|s| s.parse::<u32>().unwrap()) {
                rooms(&wd, &tex, &a[3], id);
            }
        }
        "pairs" => pairs(&wd),
        other => panic!("unknown mode {other}"),
    }
}

fn doors(wd: &WorldDisc, chunk: u16) {
    for pack in 0..50 {
        let Ok(b) = wd.file(&format!("npc/{pack}")) else { continue };
        let Some(p) = NpcPack::parse(&b) else { continue };
        let d = p.d_block(&b);
        for a in &p.actors {
            if (a.b & 0xFFFF) as u16 != chunk {
                continue;
            }
            let Ok(code) = tier2::disassemble(d, a.script) else { continue };
            let warps: Vec<String> = code.iter().filter(|i| i.op == 0x19).map(|i| format!("{:?}", i.operands)).collect();
            if warps.is_empty() {
                continue;
            }
            let at = placement_ladder(d, a.placement)
                .iter()
                .find_map(|br| match br.outcome {
                    Outcome::Place { x, h, z, dir } => Some(format!("({x}, {h}, {z}) dir {dir}")),
                    _ => None,
                })
                .unwrap_or_else(|| "-".into());
            println!("npc/{pack} actor {} type {} at {at}: warp {}", a.slot, (a.d >> 8) & 0xFF, warps.join(" | "));
        }
    }
}

/// Walkable regions: 4-connected cells with h < −1 whose steps are ≤ 32.
fn regions(c: &Chunk) -> Vec<(Vec<usize>, i16)> {
    let walk = |i: usize| c.heights[i] < -1;
    let mut seen = vec![false; 128 * 128];
    let mut out = Vec::new();
    for start in 0..128 * 128 {
        if seen[start] || !walk(start) {
            continue;
        }
        let mut cells = Vec::new();
        let mut q = VecDeque::from([start]);
        seen[start] = true;
        let mut floor: BTreeMap<i16, usize> = BTreeMap::new();
        while let Some(i) = q.pop_front() {
            cells.push(i);
            *floor.entry(c.heights[i]).or_default() += 1;
            let (x, z) = ((i % 128) as i32, (i / 128) as i32);
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, nz) = (x + dx, z + dz);
                if !(0..128).contains(&nx) || !(0..128).contains(&nz) {
                    continue;
                }
                let j = (nz * 128 + nx) as usize;
                if !seen[j] && walk(j) && (c.heights[j] - c.heights[i]).abs() <= 32 {
                    seen[j] = true;
                    q.push_back(j);
                }
            }
        }
        let top = floor.iter().max_by_key(|e| e.1).map(|e| *e.0).unwrap_or(0);
        out.push((cells, top));
    }
    out
}

fn bbox(cells: &[usize]) -> (usize, usize, usize, usize) {
    let (mut c0, mut r0, mut c1, mut r1) = (127, 127, 0, 0);
    for &i in cells {
        let (c, r) = (i % 128, i / 128);
        (c0, r0, c1, r1) = (c0.min(c), r0.min(r), c1.max(c), r1.max(r));
    }
    (c0, r0, c1, r1)
}

fn verts(c: &Chunk, p: &Prim) -> Vec<[i16; 3]> {
    p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]).collect()
}

/// Floor (level), wall (upright, x or z constant) or other.
fn kind(vs: &[[i16; 3]]) -> &'static str {
    let same = |k: usize| vs.iter().all(|v| v[k] == vs[0][k]);
    if same(1) {
        "floor"
    } else if same(0) || same(2) {
        "wall"
    } else {
        "other"
    }
}

fn rooms(wd: &WorldDisc, tex: &WorldTextures, out: &str, id: u32) {
    let raw = wd.chunk(id).unwrap();
    let c = Chunk::parse(&raw).unwrap();
    let vram = tex.vram_for(wd, id, &raw);
    println!("== chunk {id}: {} groups, {} polygons, page x {} y {} h {}", c.groups.len(), c.groups.iter().map(|g| g.prims.len()).sum::<usize>(), c.texture.image.x, c.texture.image.y, c.texture.image.h);
    for (cells, top) in regions(&c).iter().filter(|r| r.0.len() >= 40) {
        let (c0, r0, c1, r1) = bbox(cells);
        println!("  region {}×{} cells ({} walkable) floor h {top} at col {c0}–{c1} row {r0}–{r1}", c1 - c0 + 1, r1 - r0 + 1, cells.len());
    }
    // Textured polygons by (tpage, clut, uv box) and kind.
    let mut groups: BTreeMap<(u16, u16, [u8; 4]), (BTreeMap<&str, usize>, f32, [i16; 3])> = BTreeMap::new();
    for g in &c.groups {
        for p in &g.prims {
            if !p.textured() {
                continue;
            }
            let vs = verts(&c, p);
            let n = p.n as usize;
            let uv = [
                p.uv[..n].iter().map(|u| u[0]).min().unwrap(),
                p.uv[..n].iter().map(|u| u[1]).min().unwrap(),
                p.uv[..n].iter().map(|u| u[0]).max().unwrap(),
                p.uv[..n].iter().map(|u| u[1]).max().unwrap(),
            ];
            let span = |k: usize| (vs.iter().map(|v| v[k]).max().unwrap() - vs.iter().map(|v| v[k]).min().unwrap()) as f32;
            let area = (span(0).max(span(2)) * span(1).max(1.0).max(span(2).min(span(0)))).abs();
            let e = groups.entry((p.tpage, p.clut, uv)).or_insert((BTreeMap::new(), 0.0, vs[0]));
            *e.0.entry(kind(&vs)).or_default() += 1;
            e.1 += area;
        }
    }
    let mut list: Vec<_> = groups.into_iter().collect();
    list.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap());
    let shown: Vec<_> = list.iter().take(48).collect();
    for (k, ((tpage, clut, uv), (kinds, area, at))) in shown.iter().enumerate() {
        println!("  #{k:02} tpage {tpage:#06x} clut {clut:#06x} uv {}..{} × {}..{} · {kinds:?} · area {:.0} · e.g. {at:?}", uv[0], uv[2], uv[1], uv[3], area);
    }
    // Contact sheet: 8 columns of 64×64 cells, each texture box scaled to fit.
    let (cols, cell) = (8usize, 72usize);
    let rows = shown.len().div_ceil(cols);
    let (w, h) = (cols * cell, rows.max(1) * cell);
    let mut px = vec![24u8; w * h * 4];
    for (k, ((tpage, clut, uv), _)) in shown.iter().enumerate() {
        let (ox, oy) = ((k % cols) * cell + 4, (k / cols) * cell + 4);
        let (bw, bh) = ((uv[2] - uv[0]) as usize + 1, (uv[3] - uv[1]) as usize + 1);
        let s = (64.0 / bw.max(bh) as f32).min(4.0);
        for y in 0..64usize {
            for x in 0..64usize {
                let (u, v) = ((x as f32 / s) as usize, (y as f32 / s) as usize);
                if u >= bw || v >= bh {
                    continue;
                }
                let t = vram.texel(*tpage, *clut, (uv[0] as usize + u) as u8, (uv[1] as usize + v) as u8);
                let rgb = match t {
                    Texel::Colour(c) => [c5(c), c5(c >> 5), c5(c >> 10)],
                    Texel::Clear => [255, 0, 255],
                    Texel::Water => [40, 80, 160],
                };
                let o = ((oy + y) * w + ox + x) * 4;
                px[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    let f = std::fs::File::create(format!("{out}/chunk{id}-tex.png")).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&px).unwrap();
}

/// Each village door: the house it's in (the largest piece of chunk 78 within 64 units) and the room it opens into.
fn pairs(wd: &WorldDisc) {
    let raw = wd.chunk(78).unwrap();
    let c78 = Chunk::parse(&raw).unwrap();
    let split = unbake::split(&c78);
    let b = wd.file("npc/6").unwrap();
    let p = NpcPack::parse(&b).unwrap();
    let d = p.d_block(&b);
    for a in &p.actors {
        if (a.b & 0xFFFF) != 78 || (a.d >> 8) & 0xFF != 2 {
            continue;
        }
        let Some((x, z)) = placement_ladder(d, a.placement).iter().find_map(|br| match br.outcome {
            Outcome::Place { x, z, .. } => Some((x as i32, z as i32)),
            _ => None,
        }) else { continue };
        let Ok(code) = tier2::disassemble(d, a.script) else { continue };
        let Some(w) = code.iter().find(|i| i.op == 0x19) else { continue };
        let ops = format!("{:?}", w.operands);
        // "[S16(facing), Chunk(id), S16(x), S16(z), U16(flags)]": the numbers in the parentheses.
        let nums: Vec<i32> = ops.split('(').skip(1).filter_map(|t| t.split(')').next()?.parse().ok()).collect();
        let (dest, ax, az) = (nums[1] as u32, nums[2], nums[3]);
        let house = split
            .pieces
            .iter()
            .filter(|pc| {
                let near = |lo: i16, hi: i16, v: i32| v >= lo as i32 - 64 && v <= hi as i32 + 64;
                near(pc.min[0], pc.max[0], x) && near(pc.min[2], pc.max[2], z)
            })
            .max_by_key(|pc| pc.prims.len());
        let hs = house.map(|pc| format!("{}×{} units ({}×{} modules) · {} polys", pc.max[0] - pc.min[0], pc.max[2] - pc.min[2], (pc.max[0] - pc.min[0]) as f32 / 128.0, (pc.max[2] - pc.min[2]) as f32 / 128.0, pc.prims.len())).unwrap_or("-".into());
        let rc = Chunk::parse(&wd.chunk(dest).unwrap()).unwrap();
        let cell = (((az + 2048) >> 5) * 128 + ((ax + 2048) >> 5)) as usize;
        let room = regions(&rc).into_iter().find(|r| r.0.contains(&cell)).map(|(cells, top)| {
            let (c0, r0, c1, r1) = bbox(&cells);
            format!("{}×{} cells ({:.1}×{:.1} modules) floor h {top}", c1 - c0 + 1, r1 - r0 + 1, (c1 - c0 + 1) as f32 / 4.0, (r1 - r0 + 1) as f32 / 4.0)
        });
        println!("actor {} door at ({x}, {z}) · house {hs} → chunk {dest} ({ax}, {az}) · room {}", a.slot, room.unwrap_or("?".into()));
    }
}
