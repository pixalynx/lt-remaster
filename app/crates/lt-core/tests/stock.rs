//! The construction kit's stock library on real data (opt in with
//! `LT_TEST_DISC`): every item comes out of the retail disc, textured from
//! its own tiles, and stands in a new area; generated houses go into the
//! Knight's village. `KIT_OUT=<dir>` also writes `chunk78-houses.bin` and
//! `area-stock.bin` for pushing into PCSX-Redux.
use lt_core::edit::chunk::Chunk;
use lt_core::edit::house::{self, HouseSpec, Roof};
use lt_core::edit::prefab::{self, PackCache, Placement};
use lt_core::edit::stock;
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

fn at(col: u8, row: u8) -> Placement {
    Placement { col, row, rot: 0, ground_y: None, write_ground: false, lift: 0 }
}

#[test]
fn every_stock_item_comes_out_and_stands_in_a_new_area() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let tex = WorldTextures::load(&wd);
    let items = stock::catalogue();
    assert!(items.len() >= 70, "{} items", items.len());
    // A row of everything that isn't a house part, in one new area, sharing textures.
    let mut area = Chunk::new_area(1328);
    let mut cache = PackCache::default();
    let (mut col, mut row, mut tallest) = (2u8, 2u8, 0u8);
    for it in &items {
        let raw = wd.chunk(it.source.chunk()).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        let vram = tex.vram_for(&wd, it.source.chunk(), &raw);
        let p = stock::make(it, &c, Some(&vram)).unwrap_or_else(|e| panic!("{}: {e}", it.id));
        assert_eq!(p.stock.as_deref(), Some(it.id.as_str()));
        assert!(p.height() > 0 || it.id == "grass", "{} stands {} high", it.id, p.height());
        // Shared pages were copied: nothing samples a page the new area doesn't load.
        assert!(p.prims.iter().all(|q| !Into::<lt_core::world::Prim>::into(q.prim).textured() || q.tile.is_some()), "{} kept a shared page", it.id);
        // Alone in a fresh new area it always fits.
        let mut alone = Chunk::new_area(1329);
        prefab::place(&mut alone, &p, at(2, 2)).unwrap_or_else(|e| panic!("{}: {e}", it.id));
        alone.serialize().unwrap();
        if it.part.is_some() && it.roof != Some("orange") {
            continue;
        }
        let (w, h) = (p.area.w() as u8, p.area.h() as u8);
        if col + w > 126 {
            (col, row, tallest) = (2, row + tallest + 1, 0);
        }
        if row + h > 126 {
            break;
        }
        match prefab::place_with(&mut area, &p, at(col, row), &mut cache) {
            Ok(_) => {}
            Err(e) if e.code == "PREFAB_TEXTURE_FULL" || e.code == "PREFAB_CLUT_FULL" => {
                println!("the shared area is full at {}", it.id);
                break;
            }
            Err(e) => panic!("{}: {e}", it.id),
        }
        col += w + 1;
        tallest = tallest.max(h);
    }
    let bytes = area.serialize().unwrap();
    println!("new area: {:?}", area.sizes().unwrap());
    if let Some(dir) = std::env::var_os("KIT_OUT") {
        std::fs::write(std::path::Path::new(&dir).join("area-stock.bin"), &bytes).unwrap();
    }
}

#[test]
fn ten_pines_share_one_texture() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let tex = WorldTextures::load(&wd);
    let it = stock::item("pine").unwrap();
    let raw = wd.chunk(78).unwrap();
    let pine = stock::make(&it, &Chunk::parse(&raw).unwrap(), Some(&tex.vram_for(&wd, 78, &raw))).unwrap();
    let mut area = Chunk::new_area(1328);
    let mut cache = PackCache::default();
    let mut placed = Vec::new();
    for k in 0..10u8 {
        let before = area.groups.len();
        prefab::place_with(&mut area, &pine, at(4 + k * 6, 20), &mut cache).unwrap();
        let mut texels: Vec<_> = area.groups[before..].iter().flat_map(|g| g.prims.iter().map(|p| (p.uv, p.clut, p.tpage))).collect();
        texels.sort();
        placed.push(texels);
    }
    assert!(placed.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn generated_houses_stand_in_the_village() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let raw = wd.chunk(78).unwrap();
    let c = Chunk::parse(&raw).unwrap();
    let mut d = c.clone();
    // With the village's field VRAM, its own props are sampled where they already are.
    let vram = WorldTextures::load(&wd).vram_for(&wd, 78, &raw);
    let mut cache = PackCache::with_vram(vram.clone());
    // The village is near its geometry budget (97 %): a pine and a barrel besides the houses.
    for (id, col, row) in [("pine", 4, 100), ("barrel", 20, 100)] {
        let p = stock::make(&stock::item(id).unwrap(), &c, Some(&vram)).unwrap();
        let before = d.texture.pixels.clone();
        let r = prefab::place_with(&mut d, &p, at(col, row), &mut cache).unwrap();
        assert_eq!(r.cluts_new, 0, "{id}");
        println!("{id}: {} page bytes changed", before.iter().zip(&d.texture.pixels).filter(|(a, b)| a != b).count());
    }
    // Open ground west of the Knight's start (cells 8..60 × 80..120 hold no house).
    let specs = [
        (HouseSpec { roof: Roof::Orange, length: 3, door: Some(1), windows: true, chimney: Some(0) }, 8, 84),
        (HouseSpec { roof: Roof::Blue, length: 2, door: Some(0), windows: true, chimney: None }, 26, 84),
        (HouseSpec { roof: Roof::Red, length: 1, door: Some(0), windows: false, chimney: Some(0) }, 38, 84),
    ];
    for (spec, col, row) in specs {
        let h = house::house(spec, &c.texture).unwrap();
        let before = d.texture.pixels.clone();
        let r = prefab::place_with(&mut d, &h, at(col, row), &mut cache).unwrap();
        println!("{}: {r:?} · {} page bytes changed", h.name, before.iter().zip(&d.texture.pixels).filter(|(a, b)| a != b).count());
        assert_eq!(r.shared_texture, 0);
    }
    let out = d.serialize().unwrap();
    println!("chunk 78 with houses: {:?}", d.sizes().unwrap());
    // Nothing needed copying: the page and the palettes are retail's.
    assert!(d.texture == c.texture, "chunk 78's page changed");
    if let Some(dir) = std::env::var_os("KIT_OUT") {
        std::fs::write(std::path::Path::new(&dir).join("chunk78-houses.bin"), &out).unwrap();
    }
}
