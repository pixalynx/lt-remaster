//! Survey of the terrain tiles of retail chunks (research for the ground
//! painter). `cargo run --release --example ground_survey -- <disc.bin> <out dir> <chunk>…`
use std::collections::BTreeMap;

use lt_core::edit::chunk::Chunk;
use lt_core::edit::props;
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let out = std::path::PathBuf::from(&a[2]);
    std::fs::create_dir_all(&out).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let tex = WorldTextures::load(&wd);
    println!("texset ids: {:?}", tex.texset_ids);
    if a[3] == "sample" {
        return sample(&wd, &tex, &out);
    }
    for id in a[3..].iter().map(|s| s.parse::<u32>().unwrap()) {
        let raw = wd.chunk(id).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        println!("chunk {id}: texset {} · page x {} y {} h {}", tex.texset_for(id), c.texture.image.x, c.texture.image.y, c.texture.image.h);
        // Terrain polygons by (list, tpage, clut, uv box, size, flat/upright/slope).
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for g in &c.groups {
            for p in &g.prims {
                if !props::is_terrain(&c, p) {
                    continue;
                }
                let n = p.n as usize;
                let vs: Vec<[i16; 3]> = p.idx[..n].iter().map(|&i| c.pool[i as usize]).collect();
                let span = |k: usize| vs.iter().map(|v| v[k] as i32).max().unwrap() - vs.iter().map(|v| v[k] as i32).min().unwrap();
                let shape = if span(1) == 0 { "flat" } else if span(0) == 0 || span(2) == 0 { "wall" } else { "slope" };
                let (u0, v0) = (p.uv[..n].iter().map(|u| u[0]).min().unwrap(), p.uv[..n].iter().map(|u| u[1]).min().unwrap());
                let (u1, v1) = (p.uv[..n].iter().map(|u| u[0]).max().unwrap(), p.uv[..n].iter().map(|u| u[1]).max().unwrap());
                let key = format!(
                    "{} {} tp {:04x} clut {:04x} uv {:3},{:3}-{:3},{:3} size {}x{}x{}",
                    shape,
                    lt_core::world::LIST_NAMES[p.list as usize],
                    p.tpage,
                    p.clut,
                    u0,
                    v0,
                    u1,
                    v1,
                    span(0),
                    span(2),
                    span(1)
                );
                *kinds.entry(key).or_default() += 1;
            }
        }
        // Flat 128-unit tiles as a map: tile (u/32, v/32 on page 0x8) and the turn of its UVs.
        if std::env::var_os("MAP").is_some() {
            let mut grid = vec![String::from("  .  "); 32 * 32];
            for g in &c.groups {
                for p in &g.prims {
                    let n = p.n as usize;
                    if n != 4 || !p.textured() || !props::is_terrain(&c, p) {
                        continue;
                    }
                    let vs: Vec<[i16; 3]> = p.idx[..n].iter().map(|&i| c.pool[i as usize]).collect();
                    if vs.iter().any(|v| v[1] != vs[0][1]) {
                        continue;
                    }
                    let (x0, z0) = (vs.iter().map(|v| v[0]).min().unwrap(), vs.iter().map(|v| v[2]).min().unwrap());
                    let (tc, tr) = ((x0 as i32 + 2048) / 128, (z0 as i32 + 2048) / 128);
                    // Which UV corner sits at the tile's north-west vertex (min x, max z).
                    let nw = vs.iter().position(|v| v[0] == x0 && v[2] == z0 + 128).unwrap_or(9);
                    let (u0, v0) = (p.uv[..n].iter().map(|u| u[0]).min().unwrap(), p.uv[..n].iter().map(|u| u[1]).min().unwrap());
                    let corner = if nw < 4 { let u = p.uv[nw]; ((u[0] != u0) as u8) | ((u[1] != v0) as u8) << 1 } else { 9 };
                    let pg = if p.tpage & 0x1F == 8 { ' ' } else { '*' };
                    grid[(31 - tr as usize) * 32 + tc as usize] = format!("{pg}{}{}{} ", u0 / 32, v0 / 32, corner);
                }
            }
            for r in 0..32 {
                println!("  {}", grid[r * 32..r * 32 + 32].concat());
            }
        }
        let mut v: Vec<_> = kinds.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        for (k, n) in v.iter().take(60) {
            println!("  {n:4} {k}");
        }
        println!("  … {} kinds", v.len());
        // The two shared pages as the chunk sees them, 2×, every CLUT the terrain uses side by side.
        let vram = tex.vram_for(&wd, id, &raw);
        for page in [0x8u16, 0x9, 0x18, 0x19] {
            let cluts: Vec<u16> = {
                let mut s: Vec<u16> = c.groups.iter().flat_map(|g| g.prims.iter()).filter(|p| p.textured() && (p.tpage & 0x1F) == page && props::is_terrain(&c, p)).map(|p| p.clut).collect();
                s.sort();
                s.dedup();
                s
            };
            if cluts.is_empty() {
                continue;
            }
            println!("  page {page:#x} cluts {:x?}", cluts);
            let (px, py) = ((page as usize & 0xF) * 64, ((page as usize >> 4) & 1) * 256);
            let w = 256 * cluts.len();
            let mut rgba = vec![0u8; w * 256 * 4];
            for (k, &clut) in cluts.iter().enumerate() {
                let (cx, cy) = ((clut as usize & 0x3F) * 16, clut as usize >> 6);
                for y in 0..256 {
                    for x in 0..256 {
                        let hw = vram.get(px + x / 4, py + y);
                        let i = (hw >> ((x & 3) * 4)) & 0xF;
                        let col = vram.get(cx + i as usize, cy);
                        let o = (y * w + k * 256 + x) * 4;
                        rgba[o] = lt_core::vram::c5(col);
                        rgba[o + 1] = lt_core::vram::c5(col >> 5);
                        rgba[o + 2] = lt_core::vram::c5(col >> 10);
                        rgba[o + 3] = 255;
                    }
                }
            }
            let f = std::fs::File::create(out.join(format!("chunk{id}-page{page:x}.png"))).unwrap();
            let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, 256);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
    }
}

/// A demo map in new area 1328, rendered from above and from the south-west.
fn sample(wd: &WorldDisc, tex: &WorldTextures, out: &std::path::Path) {
    use lt_core::edit::ground::{self, GroundMap, GroundTile, Material};
    let mut map = GroundMap::default();
    let mut set = |col: usize, row: usize, m: Material, level: i8, ramp: u8| map.tiles[row * 32 + col] = GroundTile { material: m as u8, level, ramp };
    for row in 2..30 {
        for col in 2..30 {
            set(col, row, Material::Grass, 0, 0);
        }
    }
    // A dirt crossroads with a square.
    for i in 2..30 {
        set(i, 14, Material::Dirt, 0, 0);
        set(15, i, Material::Dirt, 0, 0);
    }
    for row in 12..17 {
        for col in 13..18 {
            set(col, row, Material::Dirt, 0, 0);
        }
    }
    // A plateau two levels up with a ramp up its south side (one level per ramp tile).
    for row in 20..27 {
        for col in 20..28 {
            set(col, row, Material::Grass, 2, 0);
        }
    }
    set(23, 19, Material::Dirt, 1, 1);
    set(23, 18, Material::Dirt, 0, 1);
    // A pond one level down with a sand beach.
    for row in 4..10 {
        for col in 4..11 {
            set(col, row, Material::Sand, 0, 0);
        }
    }
    for row in 5..9 {
        for col in 5..10 {
            set(col, row, Material::Water, -1, 0);
        }
    }
    // Forest and rubble patches.
    for row in 20..27 {
        for col in 3..9 {
            set(col, row, Material::Forest, 0, 0);
        }
    }
    for row in 4..8 {
        for col in 22..26 {
            set(col, row, if (row + col) % 2 == 0 { Material::Rubble } else { Material::Mossy }, 0, 0);
        }
    }
    let mut c = Chunk::new_area(1328);
    let r = ground::apply(&mut c, &map).unwrap();
    println!("sample: {r:?} · sizes {:?}", c.sizes().unwrap());
    let bytes = c.serialize().unwrap();
    let vram = tex.vram_for(wd, 1328, &bytes);
    let m = lt_core::world::mesh(&bytes).unwrap();
    let mut cv = lt_core::raster::Canvas::new(1024, 1024, [0, 0, 0, 255]);
    lt_core::raster::draw_mesh(&mut cv, &m, Some(&vram), lt_core::raster::Ortho::chunk(1024), lt_core::raster::Style::Textured);
    save(&out.join("sample-top.png"), 1024, 1024, &cv.rgba);
    // From the south-west, 35° down.
    let (sn, cs) = (std::f32::consts::FRAC_PI_4.sin(), std::f32::consts::FRAC_PI_4.cos());
    let e = 35f32.to_radians();
    let mut ob = m.clone();
    for v in &mut ob.pool {
        let (x, y, z) = (v[0] as f32, v[1] as f32, v[2] as f32);
        let (rx, rz) = (x * cs - z * sn, x * sn + z * cs);
        *v = [rx.round() as i16, (y * e.sin() + rz * e.cos()).round() as i16, (-y * e.cos() + rz * e.sin()).round() as i16];
    }
    let mut cv = lt_core::raster::Canvas::new(1400, 1000, [20, 20, 24, 255]);
    let o = lt_core::raster::Ortho { scale: 1400.0 / 5900.0, left: -2950.0, top: 1900.0 };
    lt_core::raster::draw_mesh(&mut cv, &ob, Some(&vram), o, lt_core::raster::Style::Textured);
    save(&out.join("sample-oblique.png"), 1400, 1000, &cv.rgba);
}

fn save(path: &std::path::Path, w: usize, h: usize, rgba: &[u8]) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(rgba).unwrap();
}
