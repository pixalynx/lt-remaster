//! Renders the house workbench's presets (screen 23, `edit::design`) from
//! the village's textures, 3/4 from the south-west, with their costs: a
//! check that the built houses look like the retail ones.
//!
//! ```text
//! cargo run --release --example house_designs -- <disc.bin> <out dir>
//! cargo run --release --example house_designs -- <disc.bin> <out dir> styles
//! cargo run --release --example house_designs -- <disc.bin> <out.bin> showcase
//! cargo run --release --example house_designs -- <disc.bin> <out.bin> image <preset> <chunk> <x> <z> [roof]
//! cargo run --release --example house_designs -- <disc.bin> <design.json> cost
//! cargo run --release --example house_designs -- <disc.bin> - props
//! ```
//!
//! `cost` prints a saved design's problems, chip and cost; `props` the yard
//! props' footprints; `styles` renders five plans in every style, gabled and
//! hipped (`<style>-<plan>-<shape>.png`); `showcase` places the other styles'
//! presets (and the Trisken house hipped) in new area 1328 and writes a disc
//! image for PCSX-Redux.
//!
//! `image` places a preset (its roof and trim recoloured to `roof`) in
//! `chunk` the way the kit does (the chunk's field VRAM shared, ground under
//! the footprint), on the nearest flat ground clear of props to chunk-local
//! (x, z), and writes and verifies a disc image for PCSX-Redux.
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::chunk::Chunk;
use lt_core::edit::{design, house, prefab, props, stock};
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

/// The yard's props cut from the disc, and chunk 78's field VRAM for path tiles.
fn extras<'a>(wd: &WorldDisc, tex: &WorldTextures, d: &design::HouseDesign, ground: &'a lt_core::vram::Vram) -> design::Extras<'a> {
    let mut props = std::collections::HashMap::new();
    for p in d.yard.iter().flat_map(|y| y.props.iter()) {
        let item = stock::item(&p.item).unwrap();
        let n = item.source.chunk();
        let raw = wd.chunk(n).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        props.insert(p.item.clone(), stock::make(&item, &c, Some(&tex.vram_for(wd, n, &raw))).unwrap());
    }
    design::Extras { props, ground: Some(ground) }
}

/// The design's style's page.
fn style_page(wd: &WorldDisc, d: &design::HouseDesign) -> lt_core::edit::chunk::Texture {
    Chunk::parse(&wd.chunk(design::style_of(d).unwrap().chunk).unwrap()).unwrap().texture
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let raw78 = wd.chunk(house::SOURCE_CHUNK).unwrap();
    let c78 = Chunk::parse(&raw78).unwrap();
    let tex = WorldTextures::load(&wd);
    let ground = tex.vram_for(&wd, house::SOURCE_CHUNK, &raw78);
    if a.get(3).map(String::as_str) == Some("cost") {
        let d: design::HouseDesign = serde_json::from_str(&std::fs::read_to_string(&a[2]).unwrap()).unwrap();
        for p in design::validate(&d) {
            println!("{} {}: {}", if p.error { "error" } else { "warning" }, p.code, p.message);
        }
        let sp = style_page(&wd, &d);
        let pages = design::Pages { village: &c78.texture, style: &sp };
        let p = design::build(&d, &pages, &extras(&wd, &tex, &d, &ground)).unwrap();
        println!("{} · {:?}", design::chip(&d), design::cost(&d, &p, &pages));
        return;
    }
    if a.get(3).map(String::as_str) == Some("showcase") {
        let area = 1328;
        let mut c = Chunk::new_area(area);
        let blank = c.serialize().unwrap();
        let mut cache = prefab::PackCache::with_vram(tex.vram_for(&wd, area, &blank));
        let presets = design::presets();
        let mut hip = presets.iter().find(|(k, _)| k == "trisken-house").unwrap().1.clone();
        hip.roof = Some(lt_core::edit::style::RoofShape::Hip);
        hip.name = "Trisken house, hipped".into();
        let row = |k: &str| presets.iter().find(|(id, _)| id == k).unwrap().1.clone();
        // Two rows of houses either side of the middle of the area, cells (col, row).
        let spots = [(row("trisken-house"), 34, 36), (row("trisken-hall"), 60, 36), (row("claria-house"), 34, 72), (row("stone-cottage"), 60, 72), (hip, 86, 36)];
        for (d, col, r) in spots {
            let sp = style_page(&wd, &d);
            let p = design::build(&d, &design::Pages { village: &c78.texture, style: &sp }, &extras(&wd, &tex, &d, &ground)).unwrap();
            let at = prefab::Placement { col, row: r, rot: 0, ground_y: None, write_ground: false, lift: 0 };
            let placed = prefab::place_with(&mut c, &p, at, &mut cache).unwrap_or_else(|e| panic!("{}: {}", d.name, e.message));
            println!(
                "{:24} cells {col}, {r} ({} × {}): x {}..{} z {}..{} · {} polys · tiles {} · new palettes {}",
                d.name,
                p.area.w(),
                p.area.h(),
                col as i32 * 32 - 2048,
                (col as i32 + p.area.w() as i32) * 32 - 2048,
                r as i32 * 32 - 2048,
                (r as i32 + p.area.h() as i32) * 32 - 2048,
                p.prims.len(),
                placed.tiles,
                placed.cluts_new
            );
        }
        println!("mesh {} of {}", c.sizes().unwrap().mesh, lt_core::world::MESH_BUDGET);
        let staged = vec![Staged { id: format!("chunk/{area}"), bytes: Arc::new(c.serialize().unwrap()), base_sha256: None }];
        let src = Source { image: &image, exe: &exe, table: &table };
        let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: area });
        let cancel = AtomicBool::new(false);
        let out = std::path::PathBuf::from(&a[2]);
        let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
        build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
        println!("wrote {}", out.display());
        return;
    }
    if a.get(3).map(String::as_str) == Some("styles") {
        let out = std::path::PathBuf::from(&a[2]);
        std::fs::create_dir_all(&out).unwrap();
        let presets = design::presets();
        for st in lt_core::edit::style::ALL {
            for plan in ["cottage", "l-house", "t-house", "two-storey", "trisken-hall"] {
                for shape in [lt_core::edit::style::RoofShape::Gable, lt_core::edit::style::RoofShape::Hip] {
                    let mut d = presets.iter().find(|(k, _)| k == plan).unwrap().1.clone();
                    d.style = st.key.to_string();
                    d.colours = design::Colours::of(st);
                    d.roof = Some(shape);
                    let sp = style_page(&wd, &d);
                    let pages = design::Pages { village: &c78.texture, style: &sp };
                    match design::build(&d, &pages, &extras(&wd, &tex, &d, &ground)) {
                        Ok(p) => {
                            let c = design::cost(&d, &p, &pages);
                            println!("{:16} {plan:13} {shape:?}: {:4} polys · {:2} tiles · {} high", st.key, c.polygons, c.tiles, c.height);
                            std::fs::write(out.join(format!("{}-{plan}-{shape:?}.png", st.key)), prefab::thumbnail(&p, 300).unwrap()).unwrap();
                            std::fs::write(out.join(format!("{}-{plan}-{shape:?}-back.png", st.key)), prefab::thumbnail_turned(&p, 300, 2).unwrap()).unwrap();
                        }
                        Err(e) => println!("{:16} {plan:13} {shape:?}: {} {}", st.key, e.code, e.message),
                    }
                }
            }
        }
        return;
    }
    if a.get(3).map(String::as_str) == Some("inside") {
        // A test inside on the two-storey cottage: a bedroom and a kitchen with a doorway, stairs to a loft.
        use lt_core::edit::design::{Axis, Side};
        use lt_core::edit::inside::{self, Action, Furniture, Inside, InnerWall, RoomSet, Stair, Tint};
        let mut d = design::presets().into_iter().find(|(k, _)| k == "two-storey").unwrap().1;
        let room = |storey: i8, at: [u8; 2], name: &str, floor: &str, walls: &str, tint: Tint, lit: bool| RoomSet { storey, at, name: name.into(), floor: floor.into(), walls: walls.into(), tint, window_light: lit, template: None };
        let piece = |storey: i8, item: &str, at: [u8; 2], rot: u8, action: Option<Action>| Furniture { storey, item: item.into(), at, rot, action, lit: false };
        d.inside = Some(Inside {
            walls: (0..4u8).map(|z| InnerWall { storey: 0, at: [2, z], along: Axis::Z, door: z == 1 }).collect(),
            rooms: vec![
                room(0, [0, 0], "Bedroom", "rug-red", "logs", Tint::Warm, true),
                room(0, [2, 0], "Kitchen", "planks", "logs-beam", Tint::Daylight, false),
                room(1, [0, 0], "Loft", "planks", "panels-dark", Tint::Cool, true),
            ],
            furniture: vec![
                piece(0, "bed-quilt", [1, 6], 0, Some(Action::Rest { wake: Side::East })),
                piece(0, "chest", [1, 1], 0, Some(Action::Chest { item: 1, flag: 0x120 })),
                piece(0, "fireplace", [7, 8], 0, None),
                piece(0, "table-red", [11, 6], 0, Some(Action::Examine { line: "A long table, scrubbed clean.".into() })),
                piece(0, "stool", [13, 1], 0, None),
                piece(1, "bed-yellow", [2, 2], 0, Some(Action::Rest { wake: Side::North })),
            ],
            stairs: vec![Stair { storey: 0, at: [5, 1], rises: Side::North }],
            ..Inside::default()
        });
        for p in design::validate(&d) {
            println!("{} {}: {}", if p.error { "error" } else { "warning" }, p.code, p.message);
        }
        let plan = design::footprint(&d);
        let pages: std::collections::HashMap<u32, _> = [324u32, 327, 330].into_iter().map(|n| (n, Chunk::parse(&wd.chunk(n).unwrap()).unwrap().texture)).collect();
        let mut items = std::collections::HashMap::new();
        for f in &d.inside.as_ref().unwrap().furniture {
            let it = stock::item(&f.item).unwrap();
            let n = it.source.chunk();
            let raw = wd.chunk(n).unwrap();
            items.insert(f.item.clone(), stock::make(&it, &Chunk::parse(&raw).unwrap(), Some(&tex.vram_for(&wd, n, &raw))).unwrap());
        }
        // A new area per storey, from 1328 up.
        let areas: std::collections::BTreeMap<i8, u32> = inside::storeys(&d).into_iter().zip(1328u32..).collect();
        let vram = |area: u32| Some(tex.vram_for(&wd, area, &Chunk::blank(area).serialize().unwrap()));
        let built = inside::build(&d, &plan, &areas, &pages, &items, &vram).unwrap();
        for s in &built.storeys {
            println!("storey {} in {}: {} polys · {} tiles · {} palettes · {:?}", s.storey, s.area, s.polygons, s.tiles, s.palettes, s.chunk.sizes().unwrap());
        }
        for l in &built.doors {
            println!("door {l:?}");
        }
        for s in &built.spots {
            println!("spot {s:?}");
        }
        let out = std::path::PathBuf::from(&a[2]);
        std::fs::create_dir_all(&out).unwrap();
        let mut staged = Vec::new();
        for s in &built.storeys {
            let shot = prefab::harvest(&s.chunk, s.area, prefab::CellRect { c0: 0, r0: 0, c1: 127, r1: 127 }, "inside", prefab::HarvestOptions { skip_ground: false, ..Default::default() }).unwrap();
            std::fs::write(out.join(format!("inside-{}.png", s.storey)), prefab::thumbnail(&shot, 720).unwrap()).unwrap();
            staged.push(Staged { id: format!("chunk/{}", s.area), bytes: Arc::new(s.chunk.serialize().unwrap()), base_sha256: None });
        }
        let src = Source { image: &image, exe: &exe, table: &table };
        let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: 1328 });
        let cancel = AtomicBool::new(false);
        let img = out.join("inside.bin");
        let rep = build::write_image(&src, &plan, &staged, &img, &cancel, &mut |_| {}).unwrap();
        build::verify_image(&img, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
        println!("wrote {}", img.display());
        return;
    }
    if a.get(3).map(String::as_str) == Some("templates") {
        // The two-storey cottage furnished by templates: bedroom and kitchen, an inn-room loft, a cellar store under a
        // hatch. Writes each storey's picture and every finish's swatch.
        use lt_core::edit::design::{Axis, Side, Yard};
        use lt_core::edit::inside::{self, Cellar, Hatch, Inside, InnerWall, Stair};
        let mut d = design::presets().into_iter().find(|(k, _)| k == "two-storey").unwrap().1;
        d.yard = Some(Yard { at: [0, 0], size: [5, 2], fence: false, gates: Vec::new(), patches: Vec::new(), props: Vec::new() });
        d.inside = Some(Inside {
            walls: (0..4u8).map(|z| InnerWall { storey: 0, at: [2, z], along: Axis::Z, door: z == 1 }).collect(),
            stairs: vec![Stair { storey: 0, at: [5, 1], rises: Side::North }],
            cellar: Some(Cellar { outline: vec![[0, 0, 4, 2]], hatch: Some(Hatch { at: [3, 1], side: Side::East }) }),
            ..Inside::default()
        });
        let plan = design::footprint(&d);
        for (storey, cell, t) in [(0i8, [0i16, 0i16], "bedroom"), (0, [3, 0], "kitchen"), (1, [0, 0], "inn-room"), (-1, [0, 0], "cellar-store")] {
            let room = inside::rooms(&d, &plan).into_iter().find(|r| r.storey == storey && r.cells.contains(&cell)).unwrap();
            let f = inside::apply_template(&d, &plan, storey, room.key, inside::template(t).unwrap(), &inside::item_size, &[0x130, 0x131, 0x132]).unwrap();
            let mine: Vec<String> = f.inside.furniture.iter().filter(|x| x.storey == storey).map(|x| format!("{}@{:?}r{}{}", x.item, x.at, x.rot, x.action.as_ref().map_or(String::new(), |a| format!(" {a:?}")))).collect();
            println!("{t}: {mine:?} left out {:?}", f.left);
            d.inside = Some(f.inside);
        }
        for p in design::validate(&d) {
            println!("{} {}: {}", if p.error { "error" } else { "warning" }, p.code, p.message);
        }
        let pages: std::collections::HashMap<u32, _> = [324u32, 327, 330].into_iter().map(|n| (n, Chunk::parse(&wd.chunk(n).unwrap()).unwrap().texture)).collect();
        let mut items = std::collections::HashMap::new();
        for f in &d.inside.as_ref().unwrap().furniture {
            let it = stock::item(&f.item).unwrap();
            let n = it.source.chunk();
            let raw = wd.chunk(n).unwrap();
            items.insert(f.item.clone(), stock::make(&it, &Chunk::parse(&raw).unwrap(), Some(&tex.vram_for(&wd, n, &raw))).unwrap());
        }
        let areas: std::collections::BTreeMap<i8, u32> = inside::storeys(&d).into_iter().zip(1328u32..).collect();
        let vram = |area: u32| Some(tex.vram_for(&wd, area, &Chunk::blank(area).serialize().unwrap()));
        let built = inside::build(&d, &plan, &areas, &pages, &items, &vram).unwrap();
        let out = std::path::PathBuf::from(&a[2]);
        std::fs::create_dir_all(&out).unwrap();
        for s in &built.storeys {
            println!("storey {} in {}: {} polys · {} tiles · {:?}", s.storey, s.area, s.polygons, s.tiles, s.chunk.sizes().unwrap());
            let shot = prefab::harvest(&s.chunk, s.area, prefab::CellRect { c0: 0, r0: 0, c1: 63, r1: 63 }, "inside", prefab::HarvestOptions { skip_ground: false, ..Default::default() }).unwrap();
            std::fs::write(out.join(format!("templates-{}.png", s.storey)), prefab::thumbnail(&shot, 720).unwrap()).unwrap();
        }
        for f in inside::FINISHES {
            let png = inside::finish_swatch(f, &wd.chunk(f.chunk).unwrap()).unwrap();
            std::fs::write(out.join(format!("finish-{}.png", f.id)), png).unwrap();
        }
        println!("doors {} · spots {}", built.doors.len(), built.spots.len());
        return;
    }
    if a.get(3).map(String::as_str) == Some("props") {
        for id in design::YARD_PROPS {
            let item = stock::item(id).unwrap();
            let n = item.source.chunk();
            let raw = wd.chunk(n).unwrap();
            let p = stock::make(&item, &Chunk::parse(&raw).unwrap(), Some(&tex.vram_for(&wd, n, &raw))).unwrap();
            let blocked = p.heights.iter().filter(|h| **h == prefab::PatchCell::Blocked).count();
            println!("{id:10} {} × {} cells · {} polys · {blocked} blocked", p.area.w(), p.area.h(), p.prims.len());
        }
        return;
    }
    if a.get(3).map(String::as_str) == Some("image") {
        let mut d = design::presets().into_iter().find(|(k, _)| k == &a[4]).expect("no such preset").1;
        if let Some(roof) = a.get(8) {
            d.colours.roof = design::Paint::retail(roof);
            d.colours.trim = design::Paint::retail(roof);
        }
        let sp = style_page(&wd, &d);
        let p = design::build(&d, &design::Pages { village: &c78.texture, style: &sp }, &extras(&wd, &tex, &d, &ground)).unwrap();
        let chunk: u32 = a[5].parse().unwrap();
        let (x, z): (i32, i32) = (a[6].parse().unwrap(), a[7].parse().unwrap());
        let raw = wd.chunk(chunk).unwrap();
        let mut c = Chunk::parse(&raw).unwrap();
        // The nearest footprint on one walk height, clear of every prop.
        let (w, h) = (p.area.w() as i32, p.area.h() as i32);
        let cell = |v: i32| (v + 2048).div_euclid(32);
        let ground = c.heights[(cell(z) * 128 + cell(x)) as usize];
        // Props that stand up from the ground (not the chunk's rim, not flat path decals).
        let objs: Vec<[i32; 4]> = props::objects(&c).iter().filter(|o| o.max[0] - o.min[0] < 1536 && o.max[2] - o.min[2] < 1536 && o.max[1] - o.min[1] > 8).map(|o| [cell(o.min[0] as i32), cell(o.min[2] as i32), cell(o.max[0] as i32 - 1), cell(o.max[2] as i32 - 1)]).collect();
        let free = |c0: i32, r0: i32| {
            (r0..r0 + h).all(|r| (c0..c0 + w).all(|q| (c.heights[(r * 128 + q) as usize] - ground).abs() <= 16)) && objs.iter().all(|o| o[2] < c0 || o[0] >= c0 + w || o[3] < r0 || o[1] >= r0 + h)
        };
        eprintln!("ground {ground} at cell {}, {}; {} cells within 16; {} standing props", cell(x), cell(z), c.heights.iter().filter(|v| (**v - ground).abs() <= 16).count(), objs.len());
        let mut spots: Vec<(i32, i32, i32)> = (1..127 - h).flat_map(|r| (1..127 - w).map(move |q| (q, r))).map(|(q, r)| ((q + w / 2 - cell(x)).pow(2) + (r + h / 2 - cell(z)).pow(2), q, r)).collect();
        spots.sort();
        let (_, col, row) = spots.into_iter().find(|&(_, q, r)| free(q, r)).expect("no flat, clear spot");
        let mut cache = prefab::PackCache::with_vram(tex.vram_for(&wd, chunk, &raw));
        let at = prefab::Placement { col: col as u8, row: row as u8, rot: 0, ground_y: None, write_ground: false, lift: 0 };
        let before = c.sizes().unwrap().mesh;
        let placed = prefab::place_with(&mut c, &p, at, &mut cache).unwrap();
        println!(
            "{} at cells {col}, {row} ({} × {}): x {}..{} z {}..{} · ground {} · mesh {before} → {} · tiles {} · new palettes {}",
            a[4],
            w,
            h,
            col * 32 - 2048,
            (col + w) * 32 - 2048,
            row * 32 - 2048,
            (row + h) * 32 - 2048,
            placed.ground_y,
            c.sizes().unwrap().mesh,
            placed.tiles,
            placed.cluts_new
        );
        let staged = vec![Staged { id: format!("chunk/{chunk}"), bytes: Arc::new(c.serialize().unwrap()), base_sha256: None }];
        let src = Source { image: &image, exe: &exe, table: &table };
        let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: 1328 });
        let cancel = AtomicBool::new(false);
        let out = std::path::PathBuf::from(&a[2]);
        let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
        build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
        println!("wrote {}", out.display());
        return;
    }
    for (id, d) in design::presets() {
        let sp = style_page(&wd, &d);
        let pages = design::Pages { village: &c78.texture, style: &sp };
        let p = design::build(&d, &pages, &extras(&wd, &tex, &d, &ground)).unwrap();
        let c = design::cost(&d, &p, &pages);
        println!("{id:12} {:22} {:4} polys · {:3} vertices · {:2} tiles · {} × {} cells · {} high", design::chip(&d), c.polygons, c.vertices, c.tiles, c.w, c.h, c.height);
        let out = std::path::PathBuf::from(&a[2]);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(format!("{id}.png")), prefab::thumbnail(&p, 360).unwrap()).unwrap();
    }
}
