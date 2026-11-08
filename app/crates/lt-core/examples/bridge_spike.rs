//! Spike: fix chunk 42's broken bridge (the waterfall crossing north-east of
//! San Claria) for good, and write a disc image for a walk test in PCSX-Redux.
//!
//! ```text
//! cargo run --release --example bridge_spike -- <disc.bin> dump
//! cargo run --release --example bridge_spike -- <disc.bin> mend <out.chunk>
//! cargo run --release --example bridge_spike -- <disc.bin> image <out.bin> <png dir>
//! cargo run --release --example bridge_spike -- <disc.bin> pieces <out.chunk> <png dir>
//! ```
//!
//! `pieces` makes the same repair the way screen 10's Pieces tool does
//! ([`REPAIR`], `unbake::split_cut` and `rebake`): cuts for the clean deck
//! slice, its railing, the east half's other railing and a post on each side,
//! a cut of the rest of the break that is deleted, then copies of the slices
//! and posts across the gap holding their height. (This file's "north" and
//! "south" above are the old spike's; in screen 10's top view +z is north, so
//! its south railing at z −1536 is the north one.)
//!
//! The bridge is draw groups 5 (west half) and 6 (east half): a plank deck at
//! y −288 between side beams, a railing on each side (beam y −352..−320 on
//! 32-unit posts), tall end posts at x −800..−768 and −384..−352. The break is
//! staggered: the deck stops at about x −640 and −512, the north railing at
//! −719 and −448, the south one at −640 and −433, so no slice of either half
//! has both railings.
//!
//! The fix:
//! 1. **Clean slice** (64 units, x −768..−704): the west half's deck, side
//!    beams and south railing, plus the east half's north railing (x −448..−384,
//!    moved 320 west). Railing posts come from the west half's south post and
//!    the east half's north post.
//! 2. **Out with the break**: every bridge polygon of groups 5–6 above the
//!    side beams' foot (y ≤ −272) between the end posts (x −768..−384), except
//!    faces lying on those two planes. No tool does this yet (the Pieces split
//!    fuses the west half with the cliff into one fixed piece).
//! 3. **The kit**: the slice tiled six times plus posts at x −704, −592 and
//!    −480 on both sides, as one prefab placed with [`prefab::place`] (new
//!    512-unit draw groups, the chunk's own texture page and palettes).
//! 4. **The Height brush**: [`chunk::paint`] writes the deck height (−289)
//!    into rows 9–14 (z −1760..−1568), columns 40–51 (x −768..−384); the
//!    railing rows 8 and 15 stay blocked.
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::chunk::{self, CellWrite, Chunk};
use lt_core::edit::prefab::{self, CellRect, PatchCell, Placement, Prefab, PrefabPrim};
use lt_core::edit::unbake;
use lt_core::raster::{draw_mesh, Canvas, Ortho, Style};
use lt_core::world::{Mesh, MeshGroup, Prim};
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

const CHUNK: u32 = 42;
const WEST: usize = 5;
const EAST: usize = 6;
/// Walk height of the deck (its surface is y −288).
const DECK_H: i16 = -289;
const GROUND_Y: i16 = -256;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&args[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = wd.chunk(CHUNK).unwrap();
    let c = Chunk::parse(&retail).unwrap();
    match args[2].as_str() {
        "dump" => dump(&c),
        "pieces" => {
            let (out, rep) = repair(&c);
            println!("{rep:?}");
            let (a, b) = (c.sizes().unwrap(), out.sizes().unwrap());
            println!("mesh {} → {} bytes of {}, polygons {} → {}", a.mesh, b.mesh, lt_core::world::MESH_BUDGET, a.polygons, b.polygons);
            for row in (7..=16usize).rev() {
                let r: Vec<String> = (38..=53usize).map(|col| format!("{:5}", out.heights[row * 128 + col])).collect();
                println!("r{row:3} {}", r.join(""));
            }
            let mended = Chunk::parse(&fix(&c, &retail)).unwrap();
            let differ: Vec<usize> = (0..chunk::CELLS).filter(|&k| mended.heights[k] != out.heights[k]).collect();
            println!("walk cells unlike the spike's mend: {}", differ.iter().map(|k| format!("c{}r{} {}≠{}", k % 128, k / 128, out.heights[*k], mended.heights[*k])).collect::<Vec<_>>().join(", "));
            let bytes = out.serialize().unwrap();
            std::fs::write(&args[3], &bytes).unwrap();
            let pngs = std::path::PathBuf::from(&args[4]);
            std::fs::create_dir_all(&pngs).unwrap();
            let tex = WorldTextures::load(&wd);
            let v = tex.vram_for(&wd, CHUNK, &bytes);
            std::fs::write(pngs.join("pieces-top.png"), top(&out, &v)).unwrap();
            std::fs::write(pngs.join("pieces-oblique.png"), oblique_view(&out, &v)).unwrap();
        }
        // The mended chunk alone, for a variant (`examples/variant_demo.rs`).
        "mend" => std::fs::write(&args[3], fix(&c, &retail)).unwrap(),
        "image" => {
            let fixed = fix(&c, &retail);
            let out = std::path::PathBuf::from(&args[3]);
            let pngs = std::path::PathBuf::from(&args[4]);
            std::fs::create_dir_all(&pngs).unwrap();
            let tex = WorldTextures::load(&wd);
            for (name, bytes) in [("before", &retail), ("after", &fixed)] {
                let v = tex.vram_for(&wd, CHUNK, bytes);
                let ch = Chunk::parse(bytes).unwrap();
                std::fs::write(pngs.join(format!("{name}-top.png")), top(&ch, &v)).unwrap();
                std::fs::write(pngs.join(format!("{name}-oblique.png")), oblique_view(&ch, &v)).unwrap();
            }
            let staged = vec![Staged { id: format!("chunk/{CHUNK}"), bytes: Arc::new(fixed), base_sha256: None }];
            let src = Source { image: &image, exe: &exe, table: &table };
            let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: 1328 });
            let cancel = AtomicBool::new(false);
            let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
            build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
            println!("wrote and verified {}", out.display());
        }
        other => panic!("unknown mode {other}"),
    }
}

fn verts(c: &Chunk, p: &Prim) -> Vec<[i16; 3]> {
    p.idx[..p.n as usize].iter().map(|&k| c.pool[k as usize]).collect()
}

/// Every vertex inside the box (inclusive).
fn inside(vs: &[[i16; 3]], x: [i16; 2], y: [i16; 2], z: [i16; 2]) -> bool {
    vs.iter().all(|v| (x[0]..=x[1]).contains(&v[0]) && (y[0]..=y[1]).contains(&v[1]) && (z[0]..=z[1]).contains(&v[2]))
}

fn x_plane(vs: &[[i16; 3]]) -> Option<i16> {
    vs.iter().all(|v| v[0] == vs[0][0]).then_some(vs[0][0])
}

/// Polygons of `group` inside the box, as (vertices, polygon).
fn pick(c: &Chunk, group: usize, x: [i16; 2], y: [i16; 2], z: [i16; 2], keep: impl Fn(&[[i16; 3]]) -> bool) -> Vec<(Vec<[i16; 3]>, Prim)> {
    c.groups[group].prims.iter().map(|p| (verts(c, p), *p)).filter(|(vs, _)| inside(vs, x, y, z) && keep(vs)).collect()
}

fn moved(parts: &[(Vec<[i16; 3]>, Prim)], dx: i16) -> Vec<(Vec<[i16; 3]>, Prim)> {
    parts.iter().map(|(vs, p)| (vs.iter().map(|v| [v[0] + dx, v[1], v[2]]).collect(), *p)).collect()
}

fn fix(c: &Chunk, retail: &[u8]) -> Vec<u8> {
    // 1. The clean slice and the posts.
    let north_rail = |vs: &[[i16; 3]]| vs.iter().all(|v| v[2] <= -1760 && v[1] <= -320);
    let mut slice = pick(c, WEST, [-768, -704], [-352, -272], [-1792, -1536], |vs| x_plane(vs).is_none() && !north_rail(vs));
    // Tamsoft split the deck strip z −1760..−1664 at (−704, −1717) for the break: a quad and a
    // triangle. Tiled, that vertex is a T-junction on the next copy's edge and the rasterizer
    // leaves pixel cracks, so the pair becomes one quad (the texture runs on unchanged).
    let strip = |vs: &[[i16; 3]]| vs.iter().all(|v| v[1] == -288) && vs.iter().any(|v| v[2] == -1717);
    let quad = slice.iter().position(|(vs, p)| p.n == 4 && strip(vs)).unwrap();
    let corner = slice[quad].0.iter().position(|v| v[0] == -704 && v[2] == -1717).unwrap();
    slice[quad].0[corner][2] = -1664;
    slice[quad].1.uv[corner] = [176, 31];
    slice.retain(|(vs, p)| !(p.n == 3 && strip(vs)));
    slice.extend(moved(&pick(c, EAST, [-448, -384], [-352, -320], [-1792, -1760], |vs| x_plane(vs).is_none()), -320));
    let south_post = pick(c, WEST, [-704, -672], [-320, -288], [-1568, -1536], |_| true);
    let north_post = pick(c, EAST, [-480, -448], [-320, -288], [-1792, -1760], |_| true);
    println!("slice {} polygons, south post {}, north post {}", slice.len(), south_post.len(), north_post.len());
    assert!(slice.len() >= 12 && south_post.len() == 4 && north_post.len() == 4);

    let mut parts = Vec::new();
    for k in 0..6 {
        parts.extend(moved(&slice, 64 * k));
    }
    for x0 in [-704i16, -592, -480] {
        parts.extend(moved(&south_post, x0 + 704));
        parts.extend(moved(&north_post, x0 + 480));
    }

    // 2. Out with the break.
    let mut out = c.clone();
    let mut gone = 0;
    for g in [WEST, EAST] {
        let before = out.groups[g].prims.len();
        let pool = &c.pool;
        out.groups[g].prims.retain(|p| {
            let vs: Vec<[i16; 3]> = p.idx[..p.n as usize].iter().map(|&k| pool[k as usize]).collect();
            let on_end = matches!(x_plane(&vs), Some(-768) | Some(-384));
            !(inside(&vs, [-768, -384], [-400, -272], [-1792, -1536]) && !on_end)
        });
        gone += before - out.groups[g].prims.len();
        out.recompute_centre(g);
    }
    println!("removed {gone} polygons of the broken spans");

    // 3. The kit: one prefab, placed where it was cut.
    let area = CellRect { c0: 40, r0: 8, c1: 51, r1: 15 };
    let (ox, oz) = (40 * 32 - 2048, 8 * 32 - 2048);
    let mut pool = Vec::new();
    let mut prims = Vec::new();
    for (vs, p) in &parts {
        let mut q = *p;
        for (k, v) in vs.iter().enumerate() {
            q.idx[k] = pool.len() as u16;
            pool.push([v[0] - ox, v[1] - GROUND_Y, v[2] - oz]);
        }
        prims.push(PrefabPrim { prim: q.into(), tile: None });
    }
    let middle = Prefab {
        name: "Bridge middle".into(),
        source_chunk: CHUNK,
        area,
        ground_y: GROUND_Y,
        pool,
        prims,
        tiles: Vec::new(),
        heights: vec![PatchCell::Rel(0); area.w() * area.h()],
        category: String::new(),
        stock: None,
        stock_rev: 0,
        design: None,
    };
    let placed = prefab::place(&mut out, &middle, Placement { col: 40, row: 8, rot: 0, ground_y: Some(GROUND_Y), write_ground: false, lift: 0 }).unwrap();
    println!(
        "placed {} polygons, {} vertices in {} groups (shared-page polygons {}), {} walk cells",
        placed.polygons, placed.vertices, placed.groups, placed.shared_texture, placed.cells
    );

    // 4. The Height brush: the deck height across the gap.
    let kit = out.serialize().unwrap();
    let mut writes = Vec::new();
    for row in 9..=14u32 {
        for col in 40..=51u32 {
            writes.push(CellWrite { cell: row * 128 + col, value: DECK_H as i32 });
        }
    }
    let painted = chunk::paint(retail, &kit, &writes, &[]).unwrap();
    let fixed = Chunk::parse(&painted.bytes).unwrap();
    let was = Chunk::parse(retail).unwrap();
    let changed = (0..chunk::CELLS).filter(|&k| was.heights[k] != fixed.heights[k]).count();
    println!("height brush: {} cells written, {changed} changed", writes.len());
    let (a, b) = (was.sizes().unwrap(), fixed.sizes().unwrap());
    println!("mesh {} → {} bytes of {}, polygons {} → {}", a.mesh, b.mesh, lt_core::world::MESH_BUDGET, a.polygons, b.polygons);
    for row in (7..=16usize).rev() {
        let r: Vec<String> = (38..=53usize).map(|col| format!("{:5}", fixed.heights[row * 128 + col])).collect();
        println!("r{row:3} {}", r.join(""));
    }
    painted.bytes
}

/// What a cut's piece does: copies `dc` cells east holding their height, or (`true`) a delete.
type Does = &'static [(i16, bool)];

/// The Pieces tool's repair: cuts in order (an earlier cut keeps what it takes), then what each cut's piece does.
/// Boxes are open on x and z (a face on a box's wall stays out), bands closed on y.
const REPAIR: [([i16; 3], [i16; 3], Does); 6] = [
    // North is +z (up in screen 10's top view), so the "south" railing above is the north one here.
    // The deck slice x −768..−704 with its side beams: five copies east, 2 cells apart.
    ([-768, -304, -1824], [-704, -264, -1504], &[(2, false), (4, false), (6, false), (8, false), (10, false)]),
    // Its north railing (z −1568..−1536).
    ([-768, -352, -1600], [-704, -320, -1504], &[(2, false), (4, false), (6, false), (8, false), (10, false)]),
    // The east half's south railing x −448..−384 (z −1792..−1760): five copies west.
    ([-448, -352, -1824], [-384, -320, -1728], &[(-2, false), (-4, false), (-6, false), (-8, false), (-10, false)]),
    // The west half's north post at x −704 and the east half's south post at x −480: two copies each, 4 cells apart.
    // Whole cells, as the tool draws them; each box's walls clear the post's faces on x −704 and −672, −480 and −448.
    ([-736, -319, -1600], [-640, -289, -1504], &[(4, false), (8, false)]),
    ([-512, -319, -1824], [-416, -289, -1728], &[(-4, false), (-8, false)]),
    // The rest of the break between the end posts goes.
    ([-768, -400, -1824], [-384, -264, -1504], &[(0, true)]),
];

fn repair(c: &Chunk) -> (Chunk, unbake::Rebaked) {
    let cuts: Vec<unbake::Cut> = REPAIR.iter().map(|&(min, max, _)| unbake::Cut { min, max }).collect();
    let split = unbake::split_cut(c, &cuts);
    let prints = unbake::fingerprints(c, &split);
    let mut stored = Vec::new();
    for (n, &(_, _, does)) in REPAIR.iter().enumerate() {
        let k = split.pieces.iter().position(|p| p.cut == Some(n as u16)).unwrap_or_else(|| panic!("cut {n} holds nothing"));
        println!("cut {n}: {} polygons x {}..{} y {}..{} z {}..{}", split.pieces[k].prims.len(), split.pieces[k].min[0], split.pieces[k].max[0], split.pieces[k].min[1], split.pieces[k].max[1], split.pieces[k].min[2], split.pieces[k].max[2]);
        for &(dc, delete) in does {
            stored.push(unbake::PieceMove { piece: prints[k].clone(), dc, dr: 0, turns: 0, delete, copy: !delete, hold: !delete });
        }
    }
    let (moves, named) = unbake::resolve(&prints, &stored);
    assert!(named.iter().all(|n| n.is_some()));
    unbake::rebake(c, &split, &moves).unwrap()
}

/// The bridge from above, 2 px per unit.
fn top(c: &Chunk, v: &lt_core::vram::Vram) -> Vec<u8> {
    let mesh = mesh_of(c.pool.clone(), c.groups.iter().flat_map(|g| g.prims.iter().copied()).collect());
    let (w, h) = (1200usize, 720usize);
    let mut cv = Canvas::new(w, h, [24, 24, 24, 255]);
    draw_mesh(&mut cv, &mesh, Some(v), Ortho { scale: 2.0, left: -920.0, top: -1440.0 }, Style::Textured);
    lt_core::movie::png(w as u32, h as u32, &cv.rgba)
}

/// The bridge from the south-west, tilted like the field camera.
fn oblique_view(c: &Chunk, v: &lt_core::vram::Vram) -> Vec<u8> {
    let (s, k) = (std::f32::consts::FRAC_PI_4.sin(), std::f32::consts::FRAC_PI_4.cos());
    let e = 35f32.to_radians();
    let pool: Vec<[i16; 3]> = c
        .pool
        .iter()
        .map(|v| {
            let (x, y, z) = (v[0] as f32 + 576.0, v[1] as f32, v[2] as f32 + 1664.0);
            let (rx, rz) = (x * k - z * s, x * s + z * k);
            [rx.round() as i16, (y * e.sin() + rz * e.cos()).round() as i16, (-y * e.cos() + rz * e.sin()).round() as i16]
        })
        .collect();
    let mesh = mesh_of(pool, c.groups.iter().flat_map(|g| g.prims.iter().copied()).collect());
    let (w, h) = (1200usize, 800usize);
    let mut cv = Canvas::new(w, h, [24, 24, 24, 255]);
    draw_mesh(&mut cv, &mesh, Some(v), Ortho { scale: 1.6, left: -375.0, top: 480.0 }, Style::Textured);
    lt_core::movie::png(w as u32, h as u32, &cv.rgba)
}

fn mesh_of(pool: Vec<[i16; 3]>, prims: Vec<Prim>) -> Mesh {
    let n = prims.len() as u32;
    Mesh { groups: vec![MeshGroup { index: 0, mesh_ofs: 0, centre: [0; 3], lists: [0; 8], first: 0, count: n, bbox: [0; 6] }], pool, prims, mesh_end: 0 }
}

/// Every polygon over the bridge, and the pieces the unbake split makes of it.
fn dump(c: &Chunk) {
    let split = unbake::split(c);
    for (n, p) in split.pieces.iter().enumerate() {
        if p.max[0] < -950 || p.min[0] > -200 || p.max[2] < -1900 || p.min[2] > -1400 {
            continue;
        }
        println!("piece {n}: {} polygons x {}..{} y {}..{} z {}..{}{}", p.prims.len(), p.min[0], p.max[0], p.min[1], p.max[1], p.min[2], p.max[2], if p.fixed { " fixed" } else { "" });
    }
    for (g, grp) in c.groups.iter().enumerate() {
        for (i, p) in grp.prims.iter().enumerate() {
            let vs = verts(c, p);
            if !inside(&vs, [-950, -200], [-800, 0], [-1900, -1400]) || vs.iter().all(|v| (-258..=-255).contains(&v[1])) {
                continue;
            }
            println!(
                "g{g:3} p{i:4} list {} code {:#04x} n {} tpage {:#06x} clut {:#06x} verts {:?} uv {:?} rgb {:?}",
                p.list, p.code, p.n, p.tpage, p.clut, vs, &p.uv[..p.n as usize], p.rgb[0]
            );
        }
    }
}
