//! Construction kit on real data (opt in with `LT_TEST_DISC`): a village house
//! of chunk 78 copied onto open ground next to the Knight's start. Set
//! `KIT_OUT=<dir>` to also write the chunk (`chunk78-house.bin`) for pushing
//! into PCSX-Redux; on 2026-10-05 the copy rendered fully textured there.
use lt_core::edit::chunk::Chunk;
use lt_core::edit::prefab::{self, CellRect, HarvestOptions, Placement};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn colour(c: &Chunk, p: &lt_core::world::Prim, k: usize) -> u16 {
    let t = &c.texture;
    let band = ((p.tpage >> 4) & 1) * 256 - t.image.y;
    let (x, y) = (p.uv[k][0] as usize, p.uv[k][1] as usize + band as usize);
    let b = t.pixels[y * t.image.w as usize * 2 + x / 2];
    let idx = if x & 1 == 0 { b & 15 } else { b >> 4 } as usize;
    let (r, e) = t.clut.as_ref().unwrap();
    let (row, col) = ((p.clut >> 6) as usize - r.y as usize, (p.clut & 0x3F) as usize * 16 - r.x as usize);
    e[row * r.w as usize + col + idx]
}

#[test]
fn copy_a_house_next_to_the_knight() {
    // The house's walk-0 footprint is cells 52..67 × 24..31; the area adds a 2-cell margin.
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let raw = WorldDisc { image: &image, table: &table, exe: &exe }.chunk(78).unwrap();
    let c = Chunk::parse(&raw).unwrap();
    let area = CellRect { c0: 50, r0: 22, c1: 69, r1: 33 };
    let p = prefab::harvest(&c, 78, area, "village house", HarvestOptions::default()).unwrap();
    println!("harvest: ground {} · {} polygons · {} vertices · {} tiles · {} patch cells", p.ground_y, p.prims.len(), p.pool.len(), p.tiles.len(), p.heights.iter().filter(|h| **h != prefab::PatchCell::Rel(0)).count());
    // The nearest window of plain ground (−129) that fits the footprint, near the Knight at cell (63, 36).
    let (fw, fh) = prefab::footprint(&p, 0);
    let free = |col: usize, row: usize| (row..row + fh).all(|r| (col..col + fw).all(|cc| c.heights[r * 128 + cc] == -129));
    let mut best = None;
    for row in 0..=128 - fh {
        for col in 0..=128 - fw {
            if free(col, row) {
                let d = (col as i32 + fw as i32 / 2 - 63).pow(2) + (row as i32 + fh as i32 / 2 - 36).pow(2);
                if best.is_none_or(|(bd, _, _)| d < bd) {
                    best = Some((d, col, row));
                }
            }
        }
    }
    let (_, col, row) = best.expect("a free window");
    let mut d = c.clone();
    let r = prefab::place(&mut d, &p, Placement { col: col as u8, row: row as u8, rot: 0, ground_y: None, write_ground: false, lift: 0 }).unwrap();
    println!("placed at cell {col},{row}: {r:?}");
    // Every placed own-page polygon samples the colours its source did (compared as multisets of
    // corner colours: placement regroups polygons into 512-unit draw groups).
    let own = |q: &lt_core::world::Prim, ch: &Chunk| q.textured() && (q.tpage & 0xF) * 64 == ch.texture.image.x;
    let (ox, oz) = (50 * 32 - 2048, 22 * 32 - 2048);
    let picked: Vec<&lt_core::world::Prim> = c
        .groups
        .iter()
        .flat_map(|g| g.prims.iter())
        .filter(|s| {
            let n = s.n as usize;
            let cx = s.idx[..n].iter().map(|&i| c.pool[i as usize][0] as i32).sum::<i32>() / n as i32;
            let cz = s.idx[..n].iter().map(|&i| c.pool[i as usize][2] as i32).sum::<i32>() / n as i32;
            (ox..ox + 20 * 32).contains(&cx) && (oz..oz + 12 * 32).contains(&cz) && !s.idx[..n].iter().all(|&i| (c.pool[i as usize][1] - p.ground_y).abs() <= 2)
        })
        .collect();
    assert_eq!(picked.len(), p.prims.len());
    let corners = |ch: &Chunk, qs: &[&lt_core::world::Prim]| {
        let mut v: Vec<Vec<u16>> = qs.iter().filter(|q| own(q, ch)).map(|q| (0..q.n as usize).map(|k| colour(ch, q, k)).collect()).collect();
        v.sort();
        v
    };
    let placed: Vec<&lt_core::world::Prim> = d.groups[c.groups.len()..].iter().flat_map(|g| g.prims.iter()).collect();
    let (a, b) = (corners(&c, &picked), corners(&d, &placed));
    println!("own-page polygons: source {}, placed {}, identical colour sets: {}", a.len(), b.len(), a == b);
    assert_eq!(a, b);
    let out = d.serialize().unwrap();
    if let Some(dir) = std::env::var_os("KIT_OUT") {
        std::fs::write(std::path::Path::new(&dir).join("chunk78-house.bin"), &out).unwrap();
        println!("wrote chunk78-house.bin");
    }
}
