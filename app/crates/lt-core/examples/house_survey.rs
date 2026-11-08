//! Survey behind the house workbench spec (screen 23): what the generated
//! log-house parts and houses cost, how the retail L-shaped houses build their
//! roofs, and how big the retail log-house interior rooms are.
//!
//! ```text
//! cargo run --release --example house_survey -- <disc.bin> [out dir]
//! ```
//!
//! With an out dir it also writes 3/4 thumbnails of the retail houses, a
//! generated 3 × 2 log house and one of chunk 330's rooms.
use std::collections::{BTreeMap, VecDeque};

use lt_core::edit::chunk::Chunk;
use lt_core::edit::house::{self, HouseSpec, Part, Roof};
use lt_core::edit::prefab::{self, Placement, Prefab};
use lt_core::edit::stock;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn cost(name: &str, p: &Prefab) {
    let quads = p.prims.iter().filter(|q| q.prim.n == 4).count();
    println!(
        "{name:<34} {:>2}×{:<2} cells · {:>4} polys ({quads} quads) · {:>4} verts · {:>2} tiles · {:>3} high",
        p.area.w(),
        p.area.h(),
        p.prims.len(),
        p.pool.len(),
        p.tiles.len(),
        p.height()
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let c78 = Chunk::parse(&wd.chunk(78).unwrap()).unwrap();
    let page = house::page_of(&c78);

    println!("== Parts (orange)");
    for p in Part::ALL {
        cost(p.name(), &house::part(p, Roof::Orange, page).unwrap());
    }

    println!("\n== Houses (door 0, windows, chimney 0)");
    let mut houses = Vec::new();
    for n in 1..=house::MAX_LENGTH {
        let h = house::house(HouseSpec { roof: Roof::Orange, length: n, door: Some(0), windows: true, chimney: Some(0) }, page).unwrap();
        cost(&format!("log house {n}×2"), &h);
        if let (Some(out), Some(png), 3) = (a.get(2), prefab::thumbnail(&h, 512), n) {
            std::fs::write(format!("{out}/log-house-3.png"), png).unwrap();
        }
        houses.push(h);
    }

    println!("\n== Retail houses (stock)");
    let mut retail = BTreeMap::new();
    for id in ["house-blue", "house-red", "house-orange", "house-orange-l", "house-orange-long"] {
        let item = stock::item(id).unwrap();
        let p = stock::make(&item, &c78, None).unwrap();
        cost(id, &p);
        if let (Some(out), Some(png)) = (a.get(2), prefab::thumbnail(&p, 512)) {
            std::fs::write(format!("{out}/{id}.png"), png).unwrap();
        }
        retail.insert(id, p);
    }

    // Roof polygons of the L-shaped houses: anything above the eave (y < −160, Y down) that isn't
    // axis-aligned in plan has a diagonal (hip or valley) edge.
    println!("\n== L-shaped roofs");
    for id in ["house-blue", "house-orange-l"] {
        let p = &retail[id];
        let mut diagonal = 0;
        let mut roof = 0;
        let mut levels: BTreeMap<i16, usize> = BTreeMap::new();
        for q in &p.prims {
            let vs: Vec<[i16; 3]> = q.prim.idx[..q.prim.n as usize].iter().map(|&i| p.pool[i as usize]).collect();
            if vs.iter().all(|v| v[1] > -160) {
                continue;
            }
            roof += 1;
            for v in &vs {
                *levels.entry(v[1]).or_default() += 1;
            }
            let n = vs.len();
            let diag = (0..n).any(|k| {
                let (u, w) = (vs[k], vs[(k + 1) % n]);
                u[0] != w[0] && u[2] != w[2] && u[1] == w[1]
            });
            if diag {
                diagonal += 1;
                if diagonal <= 6 {
                    println!("  {id} level-diagonal poly: {vs:?}");
                }
            }
        }
        println!("  {id}: {roof} polys above the eave, {diagonal} with a level diagonal edge; vertex heights {levels:?}");
    }

    println!("\n== Placing into chunk 78 and a blank new area");
    for (name, mut c) in [("chunk 78", c78.clone()), ("new area 1328", Chunk::new_area(1328))] {
        let before = c.sizes().unwrap();
        for (k, h) in [&houses[2], &retail["house-blue"]].into_iter().enumerate() {
            let at = Placement { col: 8 + k as u8 * 24, row: 100, rot: 0, ground_y: None, write_ground: false, lift: 0 };
            match prefab::place(&mut c, h, at) {
                Ok(pl) => println!("  {name}: +{} polys, +{} verts, {} tiles, {} groups", pl.polygons, pl.vertices, pl.tiles, pl.groups),
                Err(e) => println!("  {name}: refused {} {}", e.code, e.message),
            }
        }
        let after = c.sizes().unwrap();
        println!(
            "  {name}: mesh {} → {} bytes of {} ({:.1}% → {:.1}%)",
            before.mesh,
            after.mesh,
            lt_core::world::MESH_BUDGET,
            100.0 * before.mesh as f64 / lt_core::world::MESH_BUDGET as f64,
            100.0 * after.mesh as f64 / lt_core::world::MESH_BUDGET as f64
        );
    }

    // A retail log-house room, harvested whole, to look at.
    if let Some(out) = a.get(2) {
        let c330 = Chunk::parse(&wd.chunk(330).unwrap()).unwrap();
        let area = prefab::CellRect { c0: 24, r0: 32, c1: 51, r1: 59 };
        let room = prefab::harvest(&c330, 330, area, "room", prefab::HarvestOptions::default()).unwrap();
        cost("chunk 330 room (24×24 + rim)", &room);
        let mut c = Chunk::blank(330);
        c.texture = c330.texture.clone();
        prefab::place(&mut c, &room, Placement { col: 0, row: 0, rot: 0, ground_y: Some(0), write_ground: false, lift: 0 }).unwrap();
        if let Some(png) = prefab::thumbnail(&room, 512) {
            std::fs::write(format!("{out}/room-330.png"), png).unwrap();
        }
    }

    // Retail log-house interiors: walkable regions (rooms) and their size.
    println!("\n== Interior rooms");
    for id in [326u32, 328, 329, 330, 331] {
        let c = Chunk::parse(&wd.chunk(id).unwrap()).unwrap();
        let s = c.sizes().unwrap();
        let walk = |i: usize| c.heights[i] < -1;
        let mut seen = vec![false; 128 * 128];
        let mut rooms = Vec::new();
        for start in 0..128 * 128 {
            if seen[start] || !walk(start) {
                continue;
            }
            let (mut lo, mut hi, mut n) = ([127usize, 127], [0usize, 0], 0);
            let mut q = VecDeque::from([start]);
            seen[start] = true;
            let mut floor: BTreeMap<i16, usize> = BTreeMap::new();
            while let Some(i) = q.pop_front() {
                let (x, z) = (i % 128, i / 128);
                lo = [lo[0].min(x), lo[1].min(z)];
                hi = [hi[0].max(x), hi[1].max(z)];
                n += 1;
                *floor.entry(c.heights[i]).or_default() += 1;
                for (dx, dz) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                    let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                    if !(0..128).contains(&nx) || !(0..128).contains(&nz) {
                        continue;
                    }
                    let j = nz as usize * 128 + nx as usize;
                    if !seen[j] && walk(j) && (c.heights[j] - c.heights[i]).abs() <= 32 {
                        seen[j] = true;
                        q.push_back(j);
                    }
                }
            }
            if n >= 16 {
                let top = floor.iter().max_by_key(|e| e.1).map(|e| *e.0).unwrap_or(0);
                rooms.push(format!("{}×{} cells ({n} walkable, floor h {top}) at col {}–{} row {}–{}", hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, lo[0], hi[0], lo[1], hi[1]));
            }
        }
        println!("  chunk {id}: {} polys, {} groups, mesh {:.0}%, page {}×{} at x {}", s.polygons, c.groups.len(), 100.0 * s.mesh as f64 / lt_core::world::MESH_BUDGET as f64, c.texture.image.w, c.texture.image.h, c.texture.image.x);
        for r in rooms {
            println!("    room {r}");
        }
    }
}
