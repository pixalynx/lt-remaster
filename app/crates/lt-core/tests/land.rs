//! New land over the sea (`edit::land`) against the user's own retail image.
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test land`.

use lt_core::edit::chunk::Chunk;
use lt_core::edit::ground::Material;
use lt_core::edit::land::{self, Dock, LandMap, LandTile, Neighbour};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

impl Disc {
    fn world(&self) -> WorldDisc<'_> {
        WorldDisc { image: &self.image, table: &self.table, exe: &self.exe }
    }
    fn chunk(&self, id: u32) -> Chunk {
        Chunk::parse(&self.world().chunk(id).unwrap()).unwrap()
    }
}

fn paint(m: &mut LandMap, cols: std::ops::Range<usize>, rows: std::ops::Range<usize>, material: Material, level: i8) {
    for r in rows {
        for c in cols.clone() {
            m.tiles[r * 32 + c] = LandTile { material: material as u8, level, ramp: 0, drop: 0 };
        }
    }
}

#[test]
fn open_sea_is_paintable_everywhere_and_islands_fit() {
    let Some(d) = disc() else { return };
    // Grid chunks that are all sea (walk heights all −1): 156 are sea in every tile, and five more have a few
    // tiles of something else (1,019–1,023 sea tiles); the other two (0, 201) have more besides.
    let (mut open, mut nearly) = (0, 0);
    for id in 0..324 {
        let c = d.chunk(id);
        if c.heights.iter().all(|&h| h == -1) {
            match land::sea_tiles(&c).iter().filter(|&&s| s).count() {
                1024 => open += 1,
                1000.. => nearly += 1,
                _ => {}
            }
        }
    }
    assert_eq!((open, nearly), (156, 5));
    // An island with a hill and a dock on chunk 2.
    let mut c = d.chunk(2);
    let mut m = LandMap { zone: 0x60, ..LandMap::default() };
    paint(&mut m, 8..24, 8..24, Material::Grass, 0);
    paint(&mut m, 8..24, 8..9, Material::Sand, 0);
    paint(&mut m, 14..18, 14..18, Material::Forest, 2);
    m.docks.push(Dock { slot: 0, col: 15, row: 8, side: 2 });
    let r = land::apply(&mut c, &m, &|_, _| None).unwrap();
    assert_eq!(r.tiles, 256);
    assert!(r.cliffs > 0 && r.dock_cells == 6 * 16, "{r:?}");
    let bytes = c.serialize().unwrap();
    let back = Chunk::parse(&bytes).unwrap();
    assert_eq!(back.heights[(16 * 4) * 128 + 10 * 4], -129);
    assert_eq!(back.heights[(15 * 4 + 1) * 128 + 15 * 4 + 1], -128 - 128 - 1);
}

#[test]
fn land_across_a_seam_closes_like_retail() {
    let Some(d) = disc() else { return };
    // Chunk 2 is grid (2, 0); chunk 4 is east of it. A strip of land over their seam, one level.
    let (base2, base4) = (d.chunk(2), d.chunk(4));
    let mut m2 = LandMap::default();
    paint(&mut m2, 26..32, 10..14, Material::Grass, 0);
    let mut m4 = LandMap::default();
    paint(&mut m4, 0..6, 10..14, Material::Grass, 0);
    let n4 = Neighbour::new(&base4, Some(m4.clone()));
    let n2 = Neighbour::new(&base2, Some(m2.clone()));
    let (mut c2, mut c4) = (base2.clone(), base4.clone());
    let r2 = land::apply(&mut c2, &m2, &|col, row| land::across(2, col, row).and_then(|(n, c, r)| if n == 4 { n4.at(c, r) } else { None })).unwrap();
    let r4 = land::apply(&mut c4, &m4, &|col, row| land::across(4, col, row).and_then(|(n, c, r)| if n == 2 { n2.at(c, r) } else { None })).unwrap();
    // Each side: 6 × 4 tiles, faces north, south and on the far end only: (6 + 6 + 4) sides × 2 bands.
    assert_eq!((r2.cliffs, r4.cliffs), (32, 32));
    // The seam: 2's land edge at +2051, 4's at −2048, the same height.
    let edge = |c: &Chunk, x: i16| {
        let mut v: Vec<(i16, i16)> = c.pool.iter().filter(|v| v[0] == x && v[1] < 0).map(|v| (v[2], v[1])).collect();
        v.sort();
        v
    };
    assert_eq!(edge(&c2, 2051), edge(&c4, -2048));
    assert_eq!(edge(&c2, 2051).iter().filter(|v| v.1 == -128).count(), 5);
    c2.serialize().unwrap();
    c4.serialize().unwrap();
}

#[test]
fn a_coast_takes_new_land_against_its_own() {
    let Some(d) = disc() else { return };
    // Chunk 11's coast runs diagonally; paint the sea tiles just east of its land on rows 20–23.
    let mut c = d.chunk(11);
    let sea = land::sea_tiles(&c);
    let mut m = LandMap::default();
    let mut n = 0;
    for row in 20..24 {
        let first = (0..32).find(|&col| sea[row * 32 + col]).unwrap();
        for col in first..first + 3 {
            m.tiles[row * 32 + col] = LandTile { material: Material::Grass as u8, level: 0, ramp: 0, drop: 0 };
            n += 1;
        }
    }
    let r = land::apply(&mut c, &m, &|_, _| None).unwrap();
    assert_eq!(r.tiles, n);
    c.serialize().unwrap();
}

#[test]
fn docks_patch_retail_ovr02() {
    let Some(d) = disc() else { return };
    let ovr = d.world().file("ovr/2").unwrap();
    let dock = Dock { slot: 1, col: 15, row: 8, side: 2 };
    let out = land::patch_docks(&ovr, &ovr, &[dock]).unwrap();
    assert_ne!(out, ovr);
    assert_eq!(land::patch_docks(&ovr, &out, &[]).unwrap(), ovr);
}

#[test]
fn the_world_map_draws_new_land_over_sea_only() {
    let Some(d) = disc() else { return };
    let mut m = LandMap::default();
    paint(&mut m, 8..24, 8..24, Material::Grass, 0);
    let m = std::rc::Rc::new(m);
    for (file, row, x0) in lt_core::edit::map_screen::FILES {
        let retail = d.world().file(file).unwrap();
        assert_eq!(lt_core::edit::map_screen::redraw(&retail, row, x0, &|_| None).unwrap(), retail, "{file}");
        let out = lt_core::edit::map_screen::redraw(&retail, row, x0, &|c| (c == 2).then(|| m.clone())).unwrap();
        let changed = retail.iter().zip(&out).filter(|(a, b)| a != b).count();
        assert!((20..200).contains(&changed), "{file}: {changed} bytes changed");
    }
}
