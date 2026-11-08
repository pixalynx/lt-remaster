//! Game data (screen 13) against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test gamedata`.
//! The image is opened read-only.
//!
//! `LT_DUMP_GAMEDATA=src/mock/game/13-data` also rewrites the browser mocks
//! (enemies, items, drop-remap, toys, shops, encounters, growth) from the
//! decoders.

use std::path::PathBuf;
use std::time::Instant;

use lt_core::gamedata::{GameData, Source};
use lt_core::{DiscImage, FileTable, PsxExe};
use serde_json::Value;

fn load() -> Option<GameData> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let disc = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&disc).unwrap();
    let table = FileTable::parse(&exe, disc.sectors()).unwrap();
    let t = Instant::now();
    let data = GameData::load(Source::new(&disc, &exe, &table)).unwrap();
    eprintln!("GameData::load took {:?}", t.elapsed());
    Some(data)
}

fn mock(name: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src/mock/game/13-data").join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn gamedata_matches_retail() {
    let Some(data) = load() else { return };

    // --- Enemies: Knoygn is enemy/17, profile at 0x1E8FC.
    assert_eq!(data.enemies.len(), 110);
    let k = data.enemy(17).unwrap();
    assert_eq!((k.name.as_str(), k.file, k.prof_ofs, k.raw.len()), ("Knoygn", 16, 0x1E8FC, 0x398));
    assert_eq!((k.level, k.hp_base, k.bonus), (2, 0, [0, 0, 0, 0]));
    assert_eq!((k.drop_b, k.chance_b, k.drop_a, k.chance_a), (1, 20, 0, 0));
    assert_eq!((k.dims, k.unk16, k.unk18, k.unk19, k.unk22, k.unk24), ([6, 140, 130, 70], 1, 1, 7, 80, 20));
    assert_eq!(k.atk[0].1, "Head Butt");
    assert_eq!((k.w, k.wl), ([60, 40, 0, 0, 0, 0], [40, 60, 0, 0, 0, 0]));
    assert_eq!(k.seq[..8], [0, 10, 48, 66, 78, 90, 108, 122]);
    assert_eq!(k.scr, [164, 240, 164, 164, 164, 164]);
    assert_eq!(k.mask, 0xFFFF_FFFF);
    let zones: Vec<u16> = k.homes.iter().map(|h| h.area).collect();
    assert_eq!(zones, [64, 65]);
    assert!(k.homes.iter().all(|h| h.arena == 3));
    assert_eq!(data.enemy(20).unwrap().mask, 1 << 8, "Darkbeat needs OVR08");
    for p in [31u16, 53, 79] {
        let e = data.enemy(p).unwrap();
        assert!(e.proto && e.name.is_empty() && e.raw.len() == 0x399, "prototype {p}");
    }
    assert!(data.enemies.iter().all(|e| e.species == e.id), "the id field equals the file index");
    assert_eq!(data.enemies.iter().filter(|e| e.mask != 0xFFFF_FFFF).count(), 59, "59 enemies tied to a module");

    // Every value the browser mock carries (taken from the same disc by the
    // generator scripts, masks from gc_decomp sim_catalog.h) must match.
    let ours = serde_json::to_value(&data.enemies).unwrap();
    let theirs = mock("enemies.json");
    for (a, b) in ours.as_array().unwrap().iter().zip(theirs.as_array().unwrap()) {
        for (key, v) in b.as_object().unwrap() {
            assert_eq!(&a[key], v, "enemy/{} field {key}", b["id"]);
        }
    }

    // --- Items and the drop remap.
    assert_eq!(data.items.items.len(), 215);
    let candy = &data.items.items[1];
    assert_eq!((candy.name.as_str(), candy.price, candy.cls, candy.stat, candy.val), ("Candy Bar", 10, 0x0704, 6, 30));
    assert_eq!(data.items.drop_remap.len(), 206);
    assert_eq!((data.items.drop_remap[7], data.items.drop_remap[8], data.items.drop_remap[22]), (188, 189, 20));
    let remap = mock("drop-remap.json");
    let rm: Vec<i64> = remap.as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    let ours: Vec<i64> = data.items.drop_remap.iter().map(|&v| v as i64).collect();
    assert_eq!(ours, rm);
    let items = mock("items.json");
    for (it, m) in data.items.items.iter().zip(items.as_array().unwrap()) {
        let (id, name) = (m.get("id").or_else(|| m.get(0)).unwrap(), m.get("name").or_else(|| m.get(1)).unwrap());
        assert_eq!((it.id as u64, it.name.as_str()), (id.as_u64().unwrap(), name.as_str().unwrap()));
    }

    // --- Living Toys.
    assert_eq!(data.toys.roster.len(), 72);
    let draken = &data.toys.roster[22];
    assert_eq!((draken.name.as_str(), draken.pp, draken.category, draken.target), ("Draken", 4, 7, 0x43));
    assert_eq!(draken.file.as_ref().unwrap().phases[0].name, "Hatchet");
    assert!(data.toys.roster.iter().all(|t| t.row_byte == t.row));
    let e = data.toys.roster[63].file.as_ref().unwrap();
    assert_eq!((e.phases[0].name.as_str(), e.phases[0].effect), ("イコールバランス", 0x28));
    assert_eq!(data.toys.unlisted.len(), 1);
    assert_eq!(data.toys.unlisted[0].asset, "toy/72");

    // --- Shops: measured aliasing (PRD §4.7).
    let s = &data.shops.shops;
    assert_eq!(s.len(), 23);
    assert_eq!(s[1].shared_with, [3]);
    assert_eq!(s[4].shared_with, [6, 8, 10, 21]);
    assert_eq!(s[5].shared_with, [7, 9, 11]);
    assert_eq!(s[2].items, [2, 3, 15, 16, 186]);
    assert_eq!(s[22].items, [1, 1, 1, 1, 1, 1]);
    assert!((12..=20).all(|i| s[i].items.len() == 16 && s[i].shared_with.is_empty()));
    assert_eq!(data.shops.pool_size, 252);

    // --- Encounters.
    let enc = &data.encounters;
    assert_eq!((enc.table_offset, enc.mirror_offset), (0x8FC8, 0x46AC));
    assert!(enc.mirror_identical);
    let z65 = &enc.zones[65];
    assert_eq!((z65.pairs[0], z65.pairs[1], z65.pairs[2], z65.arena, z65.area), ([17, 0], [26, 0], [10, 0], 3, 7));
    assert_eq!(enc.zones.iter().filter(|z| z.pairs[0][0] != 0).count(), 162);
    assert_eq!((enc.species[17].group, enc.species[17].level), ([1, 1, 1, 1], 2));
    assert_eq!(enc.story[0].formation[0], [21, 1]);
    assert_eq!(enc.story[8].formation[..2], [[19, 1], [20, 1]]);
    assert_eq!((enc.scripted[0].map, enc.scripted[0].arena, enc.scripted[0].species, enc.scripted[0].area), (0x11E, 44, 109, 33));
    assert_eq!(enc.story[8].module, 8, "battle 0x108 loads OVR08");

    // --- Growth.
    let g = &data.growth;
    let kn = &g.templates[0];
    assert_eq!((kn.name.as_str(), kn.max_hp, kn.max_pp, kn.level, kn.spd, kn.atk, kn.def, kn.pp_stat, kn.luck), ("Knight", 18, 5, 1, 5, 6, 7, 3, 128));
    let baby = &g.templates[1];
    assert_eq!((baby.name.as_str(), baby.max_hp, baby.spd, baby.atk, baby.def), ("Baby", 32, 15, 6, 5));
    assert!(g.exp_verified && g.exp_step == 15);
    assert_eq!((g.exp[0], g.exp[1], g.exp[9], g.exp[49]), (15, 33, 445, 616688));
    assert!(g.level_up.verified);
    assert_eq!((g.level_up.picks, g.level_up.cases.as_slice()), (3, &["atk", "def", "spd", "pp", "atk", "def", "spd"][..]));

    if let Some(dir) = std::env::var_os("LT_DUMP_GAMEDATA") {
        let dir = PathBuf::from(dir);
        let write = |name: &str, v: Value| std::fs::write(dir.join(name), serde_json::to_string(&v).unwrap()).unwrap();
        write("enemies.json", serde_json::to_value(&data.enemies).unwrap());
        write("items.json", serde_json::to_value(&data.items.items).unwrap());
        write("drop-remap.json", serde_json::to_value(&data.items.drop_remap).unwrap());
        write("toys.json", serde_json::to_value(&data.toys).unwrap());
        write("shops.json", serde_json::to_value(&data.shops).unwrap());
        write("encounters.json", serde_json::to_value(&data.encounters).unwrap());
        write("growth.json", serde_json::to_value(&data.growth).unwrap());
    }
    // `LT_DUMP_RAW=<file>`: every profile's raw bytes, `[[id, [bytes…]], …]`, for
    // checking the TS re-encode (`src/screens/13-data/encode.ts`) offline.
    if let Some(file) = std::env::var_os("LT_DUMP_RAW") {
        let raws: Vec<_> = data.enemies.iter().map(|e| (e.id, &e.raw)).collect();
        std::fs::write(file, serde_json::to_string(&raws).unwrap()).unwrap();
    }
}
