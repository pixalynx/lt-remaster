//! Moves one baked prop of a retail chunk (`edit::unbake`) and shows the
//! result, the spike behind `docs/research/12-unbake.md`.
//!
//! ```text
//! cargo run --release --example unbake_demo -- <disc.bin> <out dir> <chunk> <x> <z> <dc> <dr> [turns] [--image <out.bin>]
//! ```
//!
//! Picks the smallest piece whose bounds hold chunk-local (`x`, `z`), moves
//! it `dc` cells east and `dr` cells south, turned `turns` quarter turns,
//! and writes `before.png` / `after.png` (top view), `before-3d.png` /
//! `after-3d.png` (from the south-west) and `chunk-N.bin`. The door actors,
//! examine spots and chimney smoke of the chunk's pack within 64 units of the
//! piece move with it. `--image` also writes a disc image with both files for
//! PCSX-Redux.
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::chunk::Chunk;
use lt_core::edit::placement::{self, Place, PlaceWrite};
use lt_core::edit::unbake::{self, Move};
use lt_core::npc::NpcPack;
use lt_core::raster::{draw_mesh, Canvas, Ortho, Style};
use lt_core::trigger::Outcome;
use lt_core::world::{Mesh, MeshGroup};
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

fn save(cv: &Canvas, path: &std::path::Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), cv.w as u32, cv.h as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&cv.rgba).unwrap();
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

fn mesh(c: &Chunk, oblique_view: bool) -> Mesh {
    let prims: Vec<_> = c.groups.iter().flat_map(|g| g.prims.iter().copied()).collect();
    let n = prims.len() as u32;
    let pool = if oblique_view { oblique(&c.pool) } else { c.pool.clone() };
    Mesh { groups: vec![MeshGroup { index: 0, mesh_ofs: 0, centre: [0; 3], lists: [0; 8], first: 0, count: n, bbox: [0; 6] }], pool, prims, mesh_end: 0 }
}

fn render(c: &Chunk, vram: &lt_core::vram::Vram, out: &std::path::Path, name: &str) {
    let size = 1400;
    let mut cv = Canvas::new(size, size, [20, 20, 26, 255]);
    draw_mesh(&mut cv, &mesh(c, false), Some(vram), Ortho::chunk(size), Style::Textured);
    save(&cv, &out.join(format!("{name}.png")));
    let m = mesh(c, true);
    let mut cv = Canvas::new(size, size, [20, 20, 26, 255]);
    draw_mesh(&mut cv, &m, Some(vram), Ortho::fit(&m, size), Style::Textured);
    save(&cv, &out.join(format!("{name}-3d.png")));
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let image_out = args.iter().position(|a| a == "--image").map(|i| {
        args.remove(i);
        args.remove(i)
    });
    let image = DiscImage::open(&args[1]).unwrap();
    let out = std::path::PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).unwrap();
    let id: u32 = args[3].parse().unwrap();
    let (x, z, dc, dr): (i32, i32, i16, i16) = (args[4].parse().unwrap(), args[5].parse().unwrap(), args[6].parse().unwrap(), args[7].parse().unwrap());
    let turns: u8 = args.get(8).map_or(0, |s| s.parse().unwrap());
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let tex = WorldTextures::load(&wd);
    let raw = wd.chunk(id).unwrap();
    let c = Chunk::parse(&raw).unwrap();
    let vram = tex.vram_for(&wd, id, &raw);

    let t = std::time::Instant::now();
    let split = unbake::split(&c);
    println!("split: {} pieces, {} ground decals left in place, kinds {:?} ({} ms)", split.pieces.len(), split.ground.len(), split.kinds, t.elapsed().as_millis());
    let piece = split
        .pieces
        .iter()
        .enumerate()
        .filter(|(_, p)| x >= p.min[0] as i32 && x <= p.max[0] as i32 && z >= p.min[2] as i32 && z <= p.max[2] as i32)
        .min_by_key(|(_, p)| (p.max[0] as i32 - p.min[0] as i32) * (p.max[2] as i32 - p.min[2] as i32))
        .map(|(i, _)| i)
        .expect("no piece there");
    let p = &split.pieces[piece];
    println!("piece {piece}: {} polygons ({} decals), x {}..{} z {}..{} y {}..{}{}", p.prims.len(), p.decals, p.min[0], p.max[0], p.min[2], p.max[2], p.min[1], p.max[1], if p.fixed { " (fixed)" } else { "" });
    let m = Move { piece, dc, dr, turns, delete: false, ..Default::default() };
    let t = std::time::Instant::now();
    let (moved, rep) = unbake::rebake(&c, &split, &[m]).unwrap();
    println!("rebake ({} ms): {rep:?}", t.elapsed().as_millis());
    let (s0, s1) = (c.sizes().unwrap(), moved.sizes().unwrap());
    println!("sizes before {s0:?}\nsizes after  {s1:?}");
    render(&c, &vram, &out, "before");
    render(&moved, &vram, &out, "after");
    let bytes = moved.serialize().unwrap();
    std::fs::write(out.join(format!("chunk-{id}.bin")), &bytes).unwrap();
    // The round trip: the file parses back to the same chunk.
    assert_eq!(Chunk::parse(&bytes).unwrap().pool.len(), moved.pool.len());

    // The pack: placements of door actors (type 2), examine spots (4) and smoke (5) near the piece.
    let packs = wd.file("ovr/2").map(|o| lt_core::world::pack_table_in(&exe, &o)).unwrap_or_default();
    let mut staged = vec![Staged { id: format!("chunk/{id}"), bytes: Arc::new(bytes), base_sha256: None }];
    if let Some(pack) = lt_core::world::pack_for(&packs, id, &|_| false) {
        let npc = format!("npc/{pack}");
        let b = wd.file(&npc).unwrap();
        let np = NpcPack::parse(&b).unwrap();
        let mut writes = Vec::new();
        for (slot, a) in np.actors.iter().enumerate().skip(2) {
            let kind = (a.d >> 8) & 0xFF;
            if (a.b & 0xFFFF) as u32 != id {
                continue;
            }
            for br in np.placement(&b, slot) {
                let Outcome::Place { x, h, z, dir } = br.outcome else { continue };
                let Some((nx, nz)) = unbake::carry(&split, &m, rep.lifts[0], x as i32, z as i32, 64) else { continue };
                if matches!(kind, 2 | 4 | 5) {
                    let place = Place { x: nx as i16, h: h + rep.lifts[0], z: nz as i16, dir };
                    println!("  carries slot {slot} (type {kind}) {x},{z} → {nx},{nz}");
                    writes.push(PlaceWrite { at: br.at, place: Some(place) });
                } else {
                    println!("  actor {slot} stands near the piece at {x},{z}; left where it is");
                }
            }
        }
        if !writes.is_empty() {
            let edited = placement::apply(&b, &b, &writes).unwrap();
            staged.push(Staged { id: npc, bytes: Arc::new(edited), base_sha256: None });
        }
    }
    if let Some(path) = image_out {
        let src = Source { image: &image, exe: &exe, table: &table };
        let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: 1328 });
        let cancel = AtomicBool::new(false);
        let rep = build::write_image(&src, &plan, &staged, std::path::Path::new(&path), &cancel, &mut |_| {}).unwrap();
        build::verify_image(std::path::Path::new(&path), &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
        println!("wrote {path} ({} files staged)", staged.len());
    }
}
