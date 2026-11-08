//! World archive checks against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test world`.
//! Set `LT_TEST_OUT=<dir>` to also dump renders as raw RGBA (`<name>.rgba` +
//! `<name>.txt` with the size) for eyeballing.

use std::time::Instant;

use lt_core::raster::Style;
use lt_core::world;
use lt_core::worldmap::{self, Image, WorldDisc, WorldTextures};
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
    fn w(&self) -> WorldDisc<'_> {
        WorldDisc { image: &self.image, table: &self.table, exe: &self.exe }
    }
}

fn dump(name: &str, img: &Image) {
    let Some(dir) = std::env::var_os("LT_TEST_OUT") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::write(dir.join(format!("{name}.rgba")), &img.rgba).unwrap();
    std::fs::write(dir.join(format!("{name}.txt")), format!("{} {}", img.width, img.height)).unwrap();
}

#[test]
fn chunk39_mesh_matches_chunk_tool() {
    let Some(d) = disc() else { return };
    let c = d.w().chunk(39).unwrap();
    let s = world::stats(39, &c).unwrap();
    assert_eq!((s.groups, s.polygons, s.vertices), (53, 3880, 3635));
    assert_eq!(s.mesh_bytes, 0x29398);
    assert_eq!(s.texture_bytes, 66_592);
    assert_eq!(s.heights.len(), 28);

    // PRD 09 §6: layer A ≈ 3.1 KB, layer B ≈ 0.4 KB.
    let (a, b) = world::layer_bytes(&c);
    assert!((2900..3300).contains(&a) && (300..500).contains(&b), "layers {a} {b}");
    let m = world::mesh(&c).unwrap();
    assert_eq!(m.groups.len(), 53);
    assert_eq!(m.prims.len(), 3880);
    assert_eq!(m.pool.len(), 3635);
    assert_eq!(m.mesh_end, 0x29398);
    let per_list: Vec<u32> = (0..8).map(|k| m.groups.iter().map(|g| g.lists[k]).sum()).collect();
    assert_eq!(per_list, s.lists.to_vec());
    // Every index is inside the pool and every textured page is a field page.
    for p in &m.prims {
        assert!(p.idx[..p.n as usize].iter().all(|&i| (i as usize) < m.pool.len()));
    }
}

#[test]
fn texture_sets_and_pack_table() {
    let Some(d) = disc() else { return };
    let ids = world::texset_ids(&d.exe).unwrap();
    assert_eq!(ids.len(), 56);
    assert_eq!(&ids[..5], &[339, 340, 351, 355, 359]);
    assert_eq!(ids[55], 522);
    assert_eq!(world::texset_for(&ids, 39), 0);
    assert_eq!(world::texset_for(&ids, 408), 14);

    let ovr2 = d.w().file("ovr/2").unwrap();
    let t = world::pack_table(&ovr2);
    assert_eq!(t.len(), 243);
    let none = |_f: u16| false;
    assert_eq!(world::pack_for(&t, 39, &none), Some(4));
    assert_eq!(world::pack_for(&t, 42, &none), Some(1));
    assert_eq!(world::pack_for(&t, 87, &none), Some(10));
    assert_eq!(world::pack_for(&t, 87, &|f| f == 0xE6), Some(39));
    assert_eq!(world::pack_for(&t, 40, &none), None);
}

#[test]
fn renders() {
    let Some(d) = disc() else { return };
    let w = d.w();
    let t0 = Instant::now();
    let tex = WorldTextures::load(&w);
    assert!(tex.complete());
    eprintln!("textures: {:?}", t0.elapsed());

    let c = w.chunk(39).unwrap();
    let t0 = Instant::now();
    let v = tex.vram_for(&w, 39, &c);
    let img = worldmap::render_chunk(&c, Some(&v), 1024, Style::Textured).unwrap();
    eprintln!("chunk 39 textured 1024: {:?}", t0.elapsed());
    let covered = img.rgba.chunks_exact(4).filter(|p| p[3] != 0).count();
    assert!(covered > 1024 * 1024 * 9 / 10, "chunk 39 is fully covered ({covered})");
    dump("chunk39", &img);
    let t0 = Instant::now();
    let img = worldmap::render_chunk(&c, None, 1024, Style::Groups).unwrap();
    eprintln!("chunk 39 groups 1024: {:?}", t0.elapsed());
    dump("chunk39_groups", &img);

    if let Ok(list) = std::env::var("LT_TEST_CHUNKS") {
        for id in list.split(',').filter_map(|s| s.trim().parse::<u32>().ok()) {
            let c = w.chunk(id).unwrap();
            let v = tex.vram_for(&w, id, &c);
            dump(&format!("chunk{id}"), &worldmap::render_chunk(&c, Some(&v), 512, Style::Textured).unwrap());
        }
        return;
    }
    if std::env::var_os("LT_TEST_OUT").is_some() {
        let t0 = Instant::now();
        let img = worldmap::overview(&w, &tex, 64);
        eprintln!("overview k=64: {:?}", t0.elapsed());
        dump("overview64", &img);
        let t0 = Instant::now();
        let img = worldmap::interior_sheet(&w, &tex, 64, 20);
        eprintln!("interiors 64: {:?}", t0.elapsed());
        dump("interiors64", &img);
    }
}

/// Every chunk row and pack count equals the screen 09 mock, which was parsed
/// from the same disc by `chunk_tool.py` / `scene_harvest.py`.
#[test]
fn index_matches_the_python_oracles() {
    let Some(d) = disc() else { return };
    let t0 = Instant::now();
    let idx = worldmap::index(&d.w()).unwrap();
    eprintln!("index: {:?}", t0.elapsed());
    assert_eq!(idx.chunks.len(), 524);
    let mock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/mock/game/09-world/world.json")).unwrap())
            .unwrap();
    let mut bad = 0;
    for (row, m) in idx.chunks.iter().zip(mock["chunks"].as_array().unwrap()) {
        let m: Vec<serde_json::Value> = m.as_array().unwrap().clone();
        let n = |i: usize| m[i].as_i64().unwrap();
        let attrs: Vec<u8> = m[12].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        let tim = row.tim.as_ref().unwrap();
        let ok = row.id as i64 == n(0)
            && row.groups as i64 == n(1)
            && row.vertices as i64 == n(2)
            && row.polygons as i64 == n(3)
            && row.mesh_end as i64 == n(4)
            && (tim.bpp as i64, tim.x as i64, tim.y as i64, tim.bytes as i64) == (n(5), n(6), n(7), n(8))
            && (row.heights.distinct as i64, row.heights.min as i64, row.heights.max as i64) == (n(9), n(10), n(11))
            && row.attrs == attrs
            && row.texset as i64 == n(13)
            && row.atlas_pages as i64 == n(14)
            && row.textured == (n(17) == 1);
        if !ok {
            bad += 1;
            if bad < 6 {
                eprintln!("chunk {} differs: {:?} vs {:?}", row.id, row, m);
            }
        }
    }
    assert_eq!(bad, 0);
    for (p, m) in idx.packs.iter().zip(mock["packs"].as_array().unwrap()) {
        let m = m.as_array().unwrap();
        assert_eq!((p.row.id as u64, p.row.actors as u64, p.row.doors as u64), (m[0].as_u64().unwrap(), m[1].as_u64().unwrap(), m[2].as_u64().unwrap()), "npc/{}", p.row.id);
    }
    // Contents for the mock's "first trigger" reading equal the table pack and doors.
    let c = idx.contents(&|_| false);
    assert_eq!((c[39].pack, c[39].doors), (Some(4), Some(0)));
    assert_eq!((c[40].pack, c[40].doors), (None, None));
    assert_eq!((c[42].pack, c[42].doors), (Some(1), Some(6)));
    assert_eq!((c[342].pack, c[342].doors), (Some(1), Some(4)));
    // Marks: the records that fire, per chunk (doors agree with the count).
    assert!(c.iter().all(|x| x.marks.iter().filter(|m| m[0] == 0).count() as u32 == x.doors.unwrap_or(0)));
    assert_eq!(c[78].marks, vec![[1, -1170, -800, 4], [1, -1170, -864, 4]], "Mary's talk box on chunk 78");
}

#[test]
fn npc4_roster() {
    let Some(d) = disc() else { return };
    let b = d.w().file("npc/4").unwrap();
    let pack = lt_core::npc::NpcPack::parse(&b).unwrap();
    let r = lt_core::placement::roster(&pack, &b);
    assert_eq!(r.len(), 37);
    assert_eq!(r.iter().filter(|a| a.home == 39).count(), 35);
    let fisher = &r[5];
    assert_eq!(fisher.speaker.as_deref(), Some("Fisher"));
    assert_eq!(fisher.script, 0x12B8);
    assert_eq!(fisher.program, 0x14E4);
    assert_eq!(fisher.branches.len(), 1);
    assert_eq!(fisher.branches[0].cond, 0);
    assert_eq!(fisher.branches[0].body, lt_core::placement::Body::Place { x: -1455, h: -33, z: 1440, dir: 3072 });
    assert_eq!(fisher.walk_anim, Some(5));
    assert_eq!(fisher.lines, 6);
    let names: Vec<_> = r[2..9].iter().map(|a| a.speaker.clone().unwrap_or_default()).collect();
    assert_eq!(names, ["Jiggy", "Boy", "Poet", "Fisher", "Fisher", "Clarian", "Julia"]);
    let lines: Vec<u32> = r[2..9].iter().map(|a| a.lines).collect();
    assert_eq!(lines, [11, 8, 6, 6, 4, 5, 3]);
    let anims: Vec<Option<u16>> = r[2..10].iter().map(|a| a.walk_anim).collect();
    assert_eq!(anims, [Some(0), Some(2), Some(3), Some(5), Some(0), Some(0), Some(0), None]);
    assert_eq!(r[0].shared_with, vec![1, 2]);
    assert!(r[9].object && !r[8].object);
}

/// Packet slots on npc/1 (Rasta and the bridge): one per character, none for the party, the
/// scene director (no model), the door (kind 2) and the examine spots; the bridge actor's big
/// model (kind 0x60, 52,356 B built) takes three.
#[test]
fn npc1_packet_slots() {
    let Some(d) = disc() else { return };
    let b = d.w().file("npc/1").unwrap();
    let pack = lt_core::npc::NpcPack::parse(&b).unwrap();
    let r = lt_core::placement::roster(&pack, &b);
    let slots = |i: usize| r[i].packet_slots;
    assert_eq!((slots(0), slots(1)), (0, 0), "the party has slots of its own");
    assert_eq!((slots(5), slots(9)), (1, 1), "characters");
    assert_eq!(slots(11), 0, "a director has no model");
    assert_eq!((slots(12), slots(14)), (0, 0), "a door and an examine spot");
    assert_eq!(slots(15), 3, "a big model");
    // Slots 2–10 and 18–21 one each, slot 15 three.
    assert_eq!(r.iter().map(|a| a.packet_slots as u32).sum::<u32>(), 16);
    assert_eq!(lt_core::placement::DRAWN_AT_ONCE, 5);
}

/// Every actor record of every pack has a placement ladder that ends in an
/// unconditional branch (PRD 10 §4.5: 3,770 records, first word 0xAxxx).
#[test]
fn all_rosters_decode() {
    let Some(d) = disc() else { return };
    let (mut total, mut empty, mut open_ended, mut placed) = (0, 0, 0, 0);
    for n in 0..50 {
        let b = d.w().file(&format!("npc/{n}")).unwrap();
        let Some(pack) = lt_core::npc::NpcPack::parse(&b) else { continue };
        for a in lt_core::placement::roster(&pack, &b) {
            total += 1;
            if a.branches.is_empty() {
                empty += 1;
            } else if a.branches.last().unwrap().cond != 0 {
                open_ended += 1;
            }
            if a.branches.iter().any(|b| matches!(b.body, lt_core::placement::Body::Place { .. })) {
                placed += 1;
            }
        }
    }
    eprintln!("records {total}, no ladder {empty}, no fallback {open_ended}, placed somewhere {placed}");
    assert_eq!(total, 3770);
    assert_eq!(empty, 0);
}
